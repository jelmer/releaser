use breezyshim::tree::{MutableTree, Tree, WorkingTree};

use std::path::Path;
use std::process::Command;

#[derive(Debug)]
pub enum Error {
    TreeError(breezyshim::tree::Error),
    CratesIoError(crates_io_api::Error),
    VersionError(String),
    Other(String),
}

impl From<breezyshim::tree::Error> for Error {
    fn from(e: breezyshim::tree::Error) -> Self {
        Error::TreeError(e)
    }
}

impl From<crates_io_api::Error> for Error {
    fn from(e: crates_io_api::Error) -> Self {
        Error::CratesIoError(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match &self {
            Error::TreeError(e) => write!(f, "TreeError: {}", e),
            Error::CratesIoError(e) => write!(f, "CratesIoError: {}", e),
            Error::VersionError(e) => write!(f, "VersionError: {}", e),
            Error::Other(e) => write!(f, "Other: {}", e),
        }
    }
}

impl std::error::Error for Error {}

pub fn get_owned_crates(user: &str) -> Result<Vec<url::Url>, Error> {
    let client =
        crates_io_api::SyncClient::new(crate::USER_AGENT, std::time::Duration::from_millis(1000))
            .map_err(|e| Error::Other(format!("Unable to create crates.io client: {}", e)))?;

    let user = client.user(user)?;

    let query = crates_io_api::CratesQueryBuilder::new().user_id(user.id);

    let owned_crates = client.crates(query.build())?;

    Ok(owned_crates
        .crates
        .into_iter()
        .filter_map(|c| c.repository)
        .map(|r| url::Url::parse(r.as_str()).unwrap())
        .collect::<Vec<url::Url>>())
}

// Define a function to publish a Rust package using Cargo
pub fn publish(tree: &WorkingTree, subpath: &Path) -> Result<(), Error> {
    Command::new("cargo")
        .arg("publish")
        .current_dir(tree.abspath(subpath)?)
        .spawn()
        .map_err(|e| Error::Other(format!("Unable to spawn cargo publish: {}", e)))?
        .wait()
        .map_err(|e| Error::Other(format!("Unable to wait for cargo publish: {}", e)))?;
    Ok(())
}

// Define a function to update the version in the Cargo.toml file
pub fn update_version(tree: &WorkingTree, new_version: &str) -> Result<(), Error> {
    // Read the Cargo.toml file
    let cargo_toml_contents = tree.get_file_text(Path::new("Cargo.toml"))?;

    // Parse Cargo.toml as TOML
    let mut parsed_toml: toml_edit::Document =
        String::from_utf8_lossy(cargo_toml_contents.as_slice())
            .parse()
            .map_err(|e| Error::Other(format!("Unable to parse Cargo.toml: {}", e)))?;

    // Update the version field
    if let Some(package) = parsed_toml.as_table_mut().get_mut("package") {
        if let Some(version) = package.as_table_mut().and_then(|t| t.get_mut("version")) {
            *version = toml_edit::value(new_version);
        }
    }

    // Serialize the updated TOML back to a string
    let updated_cargo_toml = parsed_toml.to_string();

    // Write the updated TOML back to Cargo.toml
    tree.put_file_bytes_non_atomic(Path::new("Cargo.toml"), updated_cargo_toml.as_bytes())?;

    Ok(())
}

// Define a function to find the version in the Cargo.toml file
pub fn find_version(tree: &dyn Tree) -> Result<crate::version::Version, Error> {
    // Read the Cargo.toml file
    let cargo_toml_contents = tree.get_file_text(Path::new("Cargo.toml"))?;

    // Parse Cargo.toml as TOML
    let parsed_toml: toml_edit::Document = String::from_utf8(cargo_toml_contents)
        .map_err(|e| Error::Other(format!("Unable to parse Cargo.toml: {}", e)))?
        .parse()
        .map_err(|e| Error::Other(format!("Unable to parse Cargo.toml: {}", e)))?;

    // Retrieve the version field
    let version = parsed_toml
        .as_table()
        .get("package")
        .and_then(|p| p.as_table())
        .and_then(|t| t.get("version"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Other("Unable to find version in Cargo.toml".to_string()))?
        .to_string();

    version
        .as_str()
        .parse()
        .map_err(|e| Error::VersionError(format!("Unable to parse version: {}", e)))
}
