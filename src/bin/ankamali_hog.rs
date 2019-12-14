//! Google Drive secret scanner in Rust
//!
//! # Usage
//! ```text
//! ankamali_hog [FLAGS] [OPTIONS] <GDRIVEID>
//!
//!FLAGS:
//!        --caseinsensitive    Sets the case insensitive flag for all regexes
//!        --entropy            Enables entropy scanning
//!        --prettyprint        Output the JSON in human readable format
//!    -v, --verbose            Sets the level of debugging information
//!    -h, --help               Prints help information
//!    -V, --version            Prints version information
//!
//!OPTIONS:
//!    -o, --outputfile <OUTPUT>    Sets the path to write the scanner results to (stdout by default)
//!        --regex <REGEX>          Sets a custom regex JSON file
//!
//!ARGS:
//!    <GDRIVEID>    The ID of the google drive file you want to scan
//! ```

#[macro_use]
extern crate clap;
extern crate google_drive3 as drive3;
extern crate hyper;
extern crate hyper_rustls;
extern crate yup_oauth2 as oauth2;

use clap::ArgMatches;
use drive3::DriveHub;
use log::{self, info};
use oauth2::{
    ApplicationSecret, Authenticator, DefaultAuthenticatorDelegate, DiskTokenStorage, FlowType,
};
use simple_error::SimpleError;
use std::path::Path;

use rusty_hogs::google_scanning::{GDriveFileInfo, GDriveScanner};
use rusty_hogs::{SecretScanner, SecretScannerBuilder};

fn main() {
    let matches = clap_app!(ankamali_hog =>
        (version: "0.4.5")
        (author: "Scott Cutler <scutler@newrelic.com>")
        (about: "Google Drive secret scanner in Rust.")
        (@arg REGEX: --regex +takes_value "Sets a custom regex JSON file")
        (@arg GDRIVEID: +required "The ID of the google drive file you want to scan")
        (@arg VERBOSE: -v --verbose ... "Sets the level of debugging information")
        (@arg ENTROPY: --entropy ... "Enables entropy scanning")
        (@arg CASE: --caseinsensitive "Sets the case insensitive flag for all regexes")
        (@arg OUTPUT: -o --outputfile +takes_value "Sets the path to write the scanner results to (stdout by default)")
        (@arg PRETTYPRINT: --prettyprint "Output the JSON in human readable format")
        (@arg OAUTHSECRETFILE: --oauthsecret "Path to an OAuth secret file (JSON) ./clientsecret.json by default")
        (@arg OAUTHTOKENFILE: --oauthtoken "Path to an OAuth token storage file ./temp_token by default")
    )
        .get_matches();
    match run(&matches) {
        Ok(()) => {}
        Err(e) => panic!("error: {}", e),
    }
}

fn run(arg_matches: &ArgMatches) -> Result<(), SimpleError> {
    // Set logging
    SecretScanner::set_logging(arg_matches.occurrences_of("VERBOSE"));

    // Initialize some variables
    let oauthsecretfile = arg_matches
        .value_of("OAUTHSECRETFILE")
        .unwrap_or_else(|| "clientsecret.json");
    let oauthtokenfile = arg_matches
        .value_of("OAUTHTOKENFILE")
        .unwrap_or_else(|| "temp_token");
    let file_id = arg_matches.value_of("GDRIVEID").unwrap();
    let scan_entropy = arg_matches.is_present("ENTROPY");
    let secret_scanner = SecretScannerBuilder::new().conf_argm(arg_matches).build();
    let gdrive_scanner = GDriveScanner::new(secret_scanner);

    // Start with GDrive auth - based on example code from drive3 API and yup-oauth2
    let secret: ApplicationSecret =
        yup_oauth2::read_application_secret(Path::new(oauthsecretfile)).expect(oauthsecretfile);
    let token_storage = DiskTokenStorage::new(&String::from(oauthtokenfile)).unwrap();
    let auth = Authenticator::new(
        &secret,
        DefaultAuthenticatorDelegate,
        hyper::Client::with_connector(hyper::net::HttpsConnector::new(
            hyper_rustls::TlsClient::new(),
        )),
        token_storage,
        Some(FlowType::InstalledInteractive),
    );
    let hub = DriveHub::new(
        hyper::Client::with_connector(hyper::net::HttpsConnector::new(
            hyper_rustls::TlsClient::new(),
        )),
        auth,
    );

    // get some initial info about the file
    let gdriveinfo = GDriveFileInfo::new(file_id, &hub).unwrap();

    // Do the scan
    let findings = gdrive_scanner.perform_scan(&gdriveinfo, &hub, scan_entropy);
    info!("Found {} secrets", findings.len());
    gdrive_scanner.secret_scanner.output_findings(&findings);

    Ok(())
}
