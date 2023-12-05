use breezyshim::tree::Tree;
use clap::Parser;
use maplit::hashmap;
use std::io::Write;
use url::Url;
use std::path::Path;
use disperse::project_config::{read_project_with_fallback, ProjectConfig};
use disperse::{find_last_version, find_last_version_in_tags};

use prometheus::{default_registry, Encoder, TextEncoder};

fn push_to_gateway(prometheus_url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    encoder.encode(&default_registry().gather(), &mut buffer)?;

    let metrics = String::from_utf8(buffer)?;

    let url = format!("{}/metrics/job/disperse", prometheus_url);
    reqwest::blocking::Client::new()
        .post(url)
        .body(metrics)
        .send()?
        .error_for_status()?;

    Ok(())
}

#[derive(Parser)]
struct Args {
    /// Print debug output
    #[clap(long)]
    debug: bool,

    /// Do not actually do anything
    #[clap(long)]
    dry_run: bool,

    /// Prometheus push gateway URL
    #[clap(long)]
    prometheus: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Release a new version of a project
    Release(ReleaseArgs),

    /// Discover projects that need to be released
    Discover(DiscoverArgs),

    /// Validate disperse configuration
    Validate(ValidateArgs),

    /// Show information about a project
    Info(InfoArgs),
}

#[derive(clap::Args)]
struct ReleaseArgs {
    #[clap(default_value = ".")]
    url: Vec<String>,

    /// New version to release
    #[clap(long)]
    new_version: Option<String>,

    /// Release, even if the CI is not passing
    #[clap(long)]
    ignore_ci: bool,
}

#[derive(clap::Args)]
struct DiscoverArgs {
    /// Pypi users to upload for
    #[clap(long, env = "PYPI_USERNAME")]
    pypi_user: Vec<String>,

    /// Crates.io users to upload for
    #[clap(long, env = "CRATES_IO_USERNAME")]
    crates_io_user: Option<String>,

    /// Force a new release, even if timeout is not reached
    #[clap(long)]
    force: bool,

    /// Display status only, do not create new releases
    #[clap(long)]
    info: bool,

    /// Just display URLs
    #[clap(long, conflicts_with = "info")]
    urls: bool,

    /// Do not exit with non-zero if projects failed to be released
    #[clap(long)]
    r#try: bool,
}

#[derive(clap::Args)]
struct ValidateArgs {
    /// Path or URL for project
    #[clap(default_value = ".")]
    path: std::path::PathBuf,
}

#[derive(clap::Args)]
struct InfoArgs {
    /// Path or URL for project
    #[clap(default_value = ".")]
    path: std::path::PathBuf,
}

pub fn info(tree: &breezyshim::tree::WorkingTree, branch: &dyn breezyshim::branch::Branch) -> i32 {
    let cfg = match disperse::project_config::read_project_with_fallback(tree) {
        Ok(cfg) => cfg,
        Err(e) => {
            log::info!("Error loading configuration: {}", e);
            return 1;
        }
    };

    let name = if let Some(name) = cfg.name.as_ref() {
        Some(name.clone())
    } else if tree.has_filename(Path::new("pyproject.toml")) {
        disperse::python::find_name_in_pyproject_toml(tree)
    } else {
        None
    };

    if let Some(name) = name {
        log::info!("Project: {}", name);
    }

    let (mut last_version, last_version_status) = if let Some((v, s)) = match find_last_version(tree, &cfg) {
        Ok(v) => v,
        Err(e) => {
            log::info!("Error loading last version: {}", e);
            return 1;
        }
    } {
        (v, s)
    } else if let Some(tag_name) = cfg.tag_name.as_deref() {
        let (v, s) = match find_last_version_in_tags(branch, tag_name) {
            Ok((Some(v), s)) => (v, s),
            Ok((None, _)) => {
                log::info!("No version found");
                return 1;
            }
            Err(e) => {
                log::info!("Error loading tags: {}", e);
                return 1;
            }
        };
        (v, s)
    } else {
        log::info!("No version found");
        return 1;
    };

    log::info!("Last release: {}", last_version.to_string());
    if let Some(status) = last_version_status {
        log::info!("  status: {}", status.to_string());
    }

    let tag_name = disperse::version::expand_tag(cfg.tag_name.as_deref().unwrap(), &last_version);
    match branch.tags().unwrap().lookup_tag(tag_name.as_str()) {
        Ok(release_revid) => {
            log::info!("  tag name: {} ({})", tag_name, release_revid);

            let rev = branch.repository().get_revision(&release_revid).unwrap();
            log::info!("  date: {}", rev.datetime().format("%Y-%m-%d %H:%M:%S"));

            if rev.revision_id != branch.last_revision() {
                let graph = branch.repository().get_graph();
                let missing = graph.iter_lefthand_ancestry(&branch.last_revision(), Some(&[release_revid.clone()])).collect::<Result<Vec<_>, _>>().unwrap();
                if missing.last().map(|r| r.is_null()).unwrap() {
                    log::info!("  last release not found in ancestry");
                } else {
                    use chrono::TimeZone;
                    let first = branch.repository().get_revision(missing.last().unwrap()).unwrap();
                    let first_timestamp = chrono::FixedOffset::east(first.timezone).timestamp(first.timestamp as i64, 0);
                    let first_age = chrono::Utc::now().signed_duration_since(first_timestamp).num_days();
                    log::info!(
                        "  {} revisions since last release. First is {} days old.",
                        missing.len(),
                        first_age,
                    );
                }
            } else {
                log::info!("  no revisions since last release");
            }
        },
        Err(NoSuchTag) => {
            log::info!("  tag {} for previous release not found", tag_name);
        },
    };

    match disperse::find_pending_version(tree, &cfg) {
        Ok(new_version) => {
            log::info!("Pending version: {}", new_version.to_string());
            0
        }
        Err(disperse::FindPendingVersionError::OddPendingVersion(e)) => {
            log::info!("Pending version: {} (odd)", e);
            1
        }
        Err(disperse::FindPendingVersionError::NotFound) => {
            disperse::version::increase_version(&mut last_version, -1);
            log::info!(
                "No pending version found; would use {}", last_version.to_string()
            );
            0
        }
        Err(NoUnreleasedChanges) => {
            log::info!("No unreleased changes");
            0
        }
    }
}

fn info_many(urls: &[Url]) -> pyo3::PyResult<i32> {
    let mut ret = 0;

    for url in urls {
        if url.to_string() != "." {
            log::info!("Processing {}", url);
        }

        let (local_wt, branch) =
            match breezyshim::controldir::ControlDir::open_tree_or_branch(url, None) {
                Ok(x) => x,
                Err(e) => {
                    ret = 1;
                    log::error!("Unable to open {}: {}", url, e);
                    continue;
                }
            };

        if let Some(wt) = local_wt {
            let lock = wt.lock_read();
            ret += info(&wt, wt.branch().as_ref());
            std::mem::drop(lock);
        } else {
            // TODO(jelmer): Just handle UnsupporedOperation
            let ws = silver_platter::workspace::Workspace::from_url(
                url,
                None,
                None,
                hashmap! {},
                hashmap! {},
                None,
                None,
                None,
            );
            let lock = ws.local_tree().lock_read();
            let r = info(&ws.local_tree(), ws.local_tree().branch().as_ref());
            std::mem::drop(lock);
            ret += r;
        }
    }
    Ok(ret)
}

fn release_many(
    urls: &[String],
    new_version: Option<String>,
    ignore_ci: bool,
    dry_run: bool,
) -> pyo3::PyResult<i32> {
    pyo3::Python::with_gil(|py| {
        let m = py.import("disperse.__main__")?;
        let release_many = m.getattr("release_many")?;
        let kwargs = pyo3::types::PyDict::new(py);
        kwargs.set_item(
            "urls",
            urls.iter().map(|u| u.to_string()).collect::<Vec<_>>(),
        )?;
        kwargs.set_item("force", true)?;
        kwargs.set_item("dry_run", dry_run)?;
        kwargs.set_item("discover", false)?;
        kwargs.set_item("new_version", new_version)?;
        kwargs.set_item("ignore_ci", ignore_ci)?;
        release_many
            .call((), Some(kwargs))?
            .extract::<Option<i32>>()
            .map(|x| x.unwrap_or(0))
    })
}

fn validate_config(path: &std::path::Path) -> pyo3::PyResult<i32> {
    let wt = breezyshim::tree::WorkingTree::open(path)?;

    let cfg = match read_project_with_fallback(&wt) {
        Ok(x) => x,
        Err(e) => {
            log::error!("Unable to read config: {}", e);
            return Ok(1);
        }
    };


    if let Some(news_file) = &cfg.news_file {
        let news_file = wt.base().join(news_file);
        if !news_file.exists() {
            log::error!("News file {} does not exist", news_file.display());
            return Ok(1);
        }
    }

    for update_version in cfg.update_version.iter() {
        match disperse::custom::validate_update_version(&wt, update_version) {
            Ok(_) => {}
            Err(e) => {
                log::error!("Invalid update_version: {}", e);
                return Ok(1);
            }
        }
    }

    for update_manpage in cfg.update_manpages.iter() {
        match disperse::manpage::validate_update_manpage(&wt, update_manpage) {
            Ok(_) => {}
            Err(e) => {
                log::error!("Invalid update_manpage: {}", e);
                return Ok(1);
            }
        }
    }

    Ok(0)
}

fn main() {
    let args = Args::parse();

    env_logger::builder()
        .format(|buf, record| writeln!(buf, "{}", record.args()))
        .filter(
            None,
            if args.debug {
                log::LevelFilter::Debug
            } else {
                log::LevelFilter::Info
            },
        )
        .init();

    let config = disperse::config::load_config().unwrap().unwrap_or_default();

    log::debug!("Config: {:?}", config);

    pyo3::prepare_freethreaded_python();

    breezyshim::init().unwrap();

    std::process::exit(match &args.command {
        Commands::Release(release_args) => {
            match release_many(
            release_args.url.as_slice(),
            release_args.new_version.clone(),
            release_args.ignore_ci,
            args.dry_run,
        ) {
            Ok(x) => x,
            Err(e) => {
                print_py_err(e);
                1
            }
        }
        }
        Commands::Discover(discover_args) => {
            let pypi_usernames = match discover_args.pypi_user.as_slice() {
                [] => config
                    .pypi
                    .map(|pypi| vec![pypi.username])
                    .unwrap_or(vec![]),
                pypi_usernames => pypi_usernames.to_vec(),
            };

            let crates_io_user = match discover_args.crates_io_user.as_ref() {
                None => config.crates_io.map(|crates_io| crates_io.username),
                Some(crates_io_user) => Some(crates_io_user.clone()),
            };

            let pypi_urls = pypi_usernames
                .iter()
                .flat_map(|pypi_username| disperse::python::pypi_discover_urls(pypi_username))
                .flatten()
                .collect::<Vec<_>>();

            let crates_io_urls = match crates_io_user {
                None => {
                    vec![]
                }
                Some(crates_io_user) => {
                    disperse::cargo::get_owned_crates(crates_io_user.as_str()).unwrap()
                }
            };

            let repositories_urls = config
                .repositories
                .and_then(|repositories| repositories.owned)
                .unwrap_or(vec![]);

            let urls: Vec<Url> = vec![pypi_urls, crates_io_urls, repositories_urls]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();

            if urls.is_empty() {
                log::error!("No projects found. Specify pypi or crates.io username, or add repositories to config");
                0
            } else {
                let ret = if discover_args.info {
                    info_many(urls.as_slice()).unwrap()
                } else if discover_args.urls {
                    println!(
                        "{}",
                        urls.iter()
                            .map(|u| u.to_string())
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                    0
                } else {
                    match release_many(
                        urls.iter()
                            .map(|x| x.to_string())
                            .collect::<Vec<_>>()
                            .as_slice(),
                        None,
                        false,
                        false,
                    ) {
                        Ok(ret) => ret,
                        Err(e) => {
                            print_py_err(e);
                            1
                        }
                    }
                };
                if let Some(prometheus) = args.prometheus {
                    push_to_gateway(prometheus.as_str()).unwrap();
                }
                if discover_args.r#try {
                    0
                } else {
                    ret
                }
            }
        }
        Commands::Validate(args) => validate_config(&args.path).unwrap(),
        Commands::Info(args) => {
            let wt = breezyshim::tree::WorkingTree::open(args.path.as_ref()).unwrap();
            info(&wt, wt.branch().as_ref())
        }
    });
}

fn print_py_err(e: pyo3::PyErr) {
    pyo3::Python::with_gil(|py| {
        if let Some(tb) = e.traceback(py) {
            println!("{}", tb.format().unwrap());
        }
    });
    panic!("{}", e);
}
