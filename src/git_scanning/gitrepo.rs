use crate::SecretScanner;
use encoding::all::ASCII;
use encoding::{DecoderTrap, Encoding};
use git2::{DiffFormat, Revwalk, Commit};
use git2::{DiffOptions, Repository, Time};
use log::{self, info};
use regex::bytes::Matches;
use serde::{Deserialize, Serialize};
use simple_error::SimpleError;
use simple_logger;
use simple_logger::init_with_level;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;
use std::str;
use tempdir::TempDir;
use url::{ParseError, Url};
use chrono::NaiveDateTime;

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash)]
pub struct GitFinding {
    //    branch: String, // this requires a walk of the commits for each finding, so lets leave it out for the moment
    pub commit: String,
    #[serde(rename = "commitHash")]
    pub commit_hash: String,
    pub date: String,
    pub diff: String,
    #[serde(rename = "stringsFound")]
    pub strings_found: Vec<String>,
    pub path: String,
    pub reason: String
}

pub enum GitScheme {
    Localpath,
    Http,
    Ssh,
    Relativepath,
    Git
}

/// Contains helper functions for performing scans of Git repositories
pub struct GitScanner {
    pub secret_scanner: SecretScanner,
    pub repo: Option<Repository>
}

/// Acts as a wrapper around a SecretScanner object to provide helper functions for performing
/// scanning against Git repositories. Relies on the [git2-rs](https://github.com/rust-lang/git2-rs)
/// library which provides lower level access to the Git data structures.
impl GitScanner {
    /// Initialize the SecretScanner object first using the SecretScannerBuilder, then provide
    /// it to this constructor method.
    pub fn new(secret_scanner: SecretScanner) -> GitScanner {
        GitScanner { secret_scanner,
                     repo: None }
    }

    pub fn perform_scan(&mut self, glob: Option<&str>, since_commit: Option<&str>, scan_entropy: bool) -> HashSet<GitFinding> {
        let repo = self.repo.as_ref().unwrap();
        let mut revwalk = repo.revwalk().unwrap();
        revwalk.push_glob("*").unwrap(); //easy mode: iterate over all the commits

        // take our "--since_commit" input (hash id) and convert it to a date and time
        let since_time_obj: Time = if since_commit.is_some() {
            let revspec = match repo.revparse(since_commit.unwrap()) {
                Ok(r) => r,
                Err(e) => panic!("SINCECOMMIT value returned an error: {:?}", e),
            };
            let o = revspec.from().unwrap();
            o.as_commit().unwrap().time()
        } else {
            Time::new(0, 0)
        };

        // convert our iterator of OIDs to commit objects
        let revwalk = revwalk.map(|id| repo.find_commit(id.unwrap())).filter(|c| c.as_ref().unwrap().time() >= since_time_obj);

        let mut findings: HashSet<GitFinding> = HashSet::new();
        // The main loop - scan each line of each diff of each commit for regex matches
        for commit in revwalk {
            // based on https://github.com/alexcrichton/git2-rs/blob/master/examples/log.rs
            let commit: Commit = commit.unwrap();
            info!("Scanning commit {}", commit.id());
            if commit.parents().len() > 1 {
                continue;
            }
            let a = if commit.parents().len() == 1 {
                let parent = commit.parent(0).unwrap();
                Some(parent.tree().unwrap())
            } else {
                None
            };
            let b = commit.tree().unwrap();
            let mut diffopts = DiffOptions::new();
            diffopts.force_binary(true);

            let diff = repo
                .diff_tree_to_tree(a.as_ref(), Some(&b), Some(&mut diffopts))
                .unwrap();

            // secondary loop that occurs for each *line* in the diff
            diff.print(DiffFormat::Patch, |delta, _hunk, line| {
                let new_line = line.content();
                let matches_map: BTreeMap<&String, Matches> = self.secret_scanner.get_matches(new_line);

                for (reason, match_iterator) in matches_map {
                    let mut secrets: Vec<String> = Vec::new();
                    for matchobj in match_iterator {
                        secrets.push(
                            ASCII
                                .decode(
                                    &new_line[matchobj.start()..matchobj.end()],
                                    DecoderTrap::Ignore,
                                )
                                .unwrap_or_else(|_| "<STRING DECODE ERROR>".parse().unwrap()),
                        );
                    }
                    if !secrets.is_empty() {
                        findings.insert(GitFinding {
                            commit_hash: commit.id().to_string(),
                            commit: commit.message().unwrap().to_string(),
                            diff: ASCII
                                .decode(&new_line, DecoderTrap::Ignore)
                                .unwrap_or_else(|_| "<STRING DECODE ERROR>".parse().unwrap()),
                            date: NaiveDateTime::from_timestamp(commit.time().seconds(), 0).to_string(),
                            strings_found: secrets.clone(),
                            path: delta
                                .new_file()
                                .path()
                                .unwrap()
                                .to_str()
                                .unwrap()
                                .to_string(),
                            reason: reason.clone(),
                        });
                    }
                }

                if scan_entropy {
                    let ef = SecretScanner::get_entropy_findings(new_line);
                    if !ef.is_empty() {
                        findings.insert(GitFinding {
                            commit: commit.message().unwrap().to_string(),
                            commit_hash: commit.id().to_string(),
                            diff: ASCII
                                .decode(&new_line, DecoderTrap::Ignore)
                                .unwrap_or_else(|_| "<STRING DECODE ERROR>".parse().unwrap()),
                            date: NaiveDateTime::from_timestamp(commit.time().seconds(), 0).to_string(),
                            strings_found: ef,
                            path: delta
                                .new_file()
                                .path()
                                .unwrap()
                                .to_str()
                                .unwrap()
                                .to_string(),
                            reason: "Entropy".to_string(),
                        });
                    }
                }
                true
            })
                .unwrap();
        }
        findings
    }

    fn get_ssh_git_repo(
        ssh_git_url: &str,
        dest_dir: &Path,
        sshkeypath: Option<&str>,
        sshkeyphrase: Option<&str>,
        username: &str,
    ) -> Repository {
        info!("username in get_ssh_git_repo: {:?}", username);
        let mut cb = git2::RemoteCallbacks::new();
        if sshkeypath.is_some() {
            cb.credentials(|_, _, _| {
                info!("SSHKEYPATH detected, attempting to read credentials from supplied path...");
                let credentials = git2::Cred::ssh_key(
                    username,
                    None,
                    Path::new(sshkeypath.unwrap()),
                    sshkeyphrase,
                )
                    .expect("Cannot create credentials object.");
                Ok(credentials)
            });
        } else {
            cb.credentials(|_, _, _| {
                info!("no SSHKEYPATH detected, attempting to read credentials from ssh_agent...");
                let credentials = git2::Cred::ssh_key_from_agent(username)
                    .expect("Cannot create credentials object from ssh_agent");
                Ok(credentials)
            });
        }
        let mut fo = git2::FetchOptions::new();
        fo.remote_callbacks(cb);
        let mut builder = git2::build::RepoBuilder::new();
        builder.fetch_options(fo);
        info!("SSH Git credentials successfully initialized, attempting to clone the repo...");
        match builder.clone(ssh_git_url, dest_dir) {
            Ok(r) => r,
            Err(e) => panic!(
                "<GITPATH> {:?} is a SSH GIT URL but couldn't be cloned:\n{:?}",
                ssh_git_url, e
            ),
        }
    }

    /// Initialize a [Repository](https://docs.rs/git2/0.10.2/git2/struct.Repository.html) object
    pub fn init_git_repo(mut self, path: &str, dest_dir: &Path, sshkeypath: Option<&str>,
                    sshkeyphrase: Option<&str>) -> GitScanner {
        let url = Url::parse(path);
        // try to figure out the format of the path
        let scheme: GitScheme = match &url {
            Ok(url) => match url.scheme().to_ascii_lowercase().as_ref() {
                "http" => {
                    info!("Git scheme detected as http://, performing a clone...");
                    GitScheme::Http
                }
                "https" => {
                    info!("Git scheme detected as https:// , performing a clone...");
                    GitScheme::Http
                }
                "file" => {
                    info!("Git scheme detected as file://, performing a clone...");
                    GitScheme::Localpath
                }
                "ssh" => {
                    info!("Git scheme detected as ssh://, performing a clone...");
                    GitScheme::Ssh
                }
                "git" => {
                    info!("Git scheme detected as git://, performing a clone...");
                    GitScheme::Git
                }
                s => panic!(
                    "Error parsing GITPATH {:?}, please include the username with \"git@\"",
                    s
                ),
            },
            Err(e) => match e {
                ParseError::RelativeUrlWithoutBase => {
                    info!(
                        "Git scheme detected as a relative path, attempting to open on the local \
                     file system and then falling back to SSH..."
                    );
                    GitScheme::Relativepath
                }
                e => panic!("Unknown error parsing GITPATH: {:?}", e),
            },
        };

        self.repo = match scheme {
            GitScheme::Localpath => match Repository::clone(path, dest_dir) {
                Ok(r) => Some(r),
                Err(e) => panic!(
                    "<GITPATH> {:?} was detected as a local path but couldn't be opened: {:?}",
                    path, e
                ),
            },
            GitScheme::Http => match Repository::clone(path, dest_dir) {
                Ok(r) => Some(r),
                Err(e) => panic!(
                    "<GITPATH> {:?} is an HTTP(s) URL but couldn't be opened: {:?}",
                    path, e
                ),
            },
            GitScheme::Git => {
                let url = url.unwrap(); // we already have assurance this passed successfully
                let username = match url.username() {
                    "" => "git",
                    s => s
                };
                Some(GitScanner::get_ssh_git_repo(path, dest_dir, sshkeypath, sshkeyphrase, username))
            }
            GitScheme::Ssh => {
                let url = url.unwrap(); // we already have assurance this passed successfully
                let username = url.username();
                Some(GitScanner::get_ssh_git_repo(path, dest_dir, sshkeypath, sshkeyphrase, username))
            }
            // since @ and : are valid characters in linux paths, we need to try both opening locally
            // and over SSH. This SSH syntax is normal for Github.
            GitScheme::Relativepath => match Repository::open(path) {
                //
                Ok(r) => Some(r),
                Err(_) => {
                    let username = match path.find('@') {
                        Some(i) => path.split_at(i).0,
                        None => "git",
                    };
                    Some(GitScanner::get_ssh_git_repo(path, dest_dir, sshkeypath, sshkeyphrase, username))
                }
            },
        };
        self
    }
}
