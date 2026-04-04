mod address;
mod agent;
mod cli;
mod config;
mod git;
mod json;
mod review;
mod util;

use std::{env, process::ExitCode};

pub fn run_from_env() -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match cli::parse(&args) {
        Ok(cli::Command::Help(text)) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Ok(cli::Command::Review(request)) => match review::run(request) {
            Ok(result) => {
                if let Some(bundle_path) = &result.bundle_path {
                    println!("Review bundle: {bundle_path}");
                    println!("Files reviewed: {}", result.files_reviewed);
                    println!("Files skipped: {}", result.files_skipped);
                    if result.files_failed > 0 {
                        println!("Files failed: {}", result.files_failed);
                    }
                    println!("Findings: {}", result.findings);
                }

                if result.success {
                    ExitCode::SUCCESS
                } else {
                    if let Some(error) = &result.error {
                        eprintln!("error: {error}");
                    }
                    ExitCode::from(1)
                }
            }
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(1)
            }
        },
        Ok(cli::Command::Address(request)) => match address::run(request.comment_id) {
            Ok(result) => {
                if let Some(bundle_path) = &result.bundle_path {
                    println!("Address bundle: {bundle_path}");
                    println!("Comments selected: {}", result.comments_selected);
                    println!("Specs generated: {}", result.specs_generated);
                    println!("Specs failed: {}", result.specs_failed);
                }

                if result.success {
                    ExitCode::SUCCESS
                } else {
                    if let Some(error) = &result.error {
                        eprintln!("error: {error}");
                    }
                    ExitCode::from(1)
                }
            }
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(1)
            }
        },
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::cli::{self, AddressRequest, Command, ReviewRequest};

    #[test]
    fn parses_explicit_review_refs() {
        let command = cli::parse(&[
            "review".to_string(),
            "--base".to_string(),
            "main".to_string(),
            "--head".to_string(),
            "feature/login".to_string(),
        ])
        .unwrap();

        assert_eq!(
            command,
            Command::Review(ReviewRequest {
                base: "main".to_string(),
                head: "feature/login".to_string(),
            })
        );
    }

    #[test]
    fn parses_triple_dot_review_refs() {
        let command =
            cli::parse(&["review".to_string(), "main...feature/login".to_string()]).unwrap();

        assert_eq!(
            command,
            Command::Review(ReviewRequest {
                base: "main".to_string(),
                head: "feature/login".to_string(),
            })
        );
    }

    #[test]
    fn rejects_ambiguous_review_args() {
        let error = cli::parse(&[
            "review".to_string(),
            "--base".to_string(),
            "main".to_string(),
            "--head".to_string(),
            "feature".to_string(),
            "main...feature".to_string(),
        ])
        .unwrap_err();

        assert!(error.contains("either --base/--head or <base>...<head>"));
    }

    #[test]
    fn parses_address_without_comment_id() {
        let command = cli::parse(&["address".to_string()]).unwrap();

        assert_eq!(
            command,
            Command::Address(AddressRequest { comment_id: None })
        );
    }

    #[test]
    fn parses_address_with_comment_id() {
        let command = cli::parse(&["address".to_string(), "R006".to_string()]).unwrap();

        assert_eq!(
            command,
            Command::Address(AddressRequest {
                comment_id: Some("R006".to_string()),
            })
        );
    }

    #[test]
    fn rejects_extra_address_args() {
        let error = cli::parse(&[
            "address".to_string(),
            "R001".to_string(),
            "R002".to_string(),
        ])
        .unwrap_err();

        assert!(error.contains("at most one positional <comment-id>"));
    }
}
