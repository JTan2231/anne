mod agent;
mod cli;
mod config;
mod git;
mod json;
mod review;

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
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::cli::{self, Command, ReviewRequest};

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
}
