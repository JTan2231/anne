pub mod address;
mod agent;
mod cli;
pub mod config;
mod filter;
pub mod git;
pub mod investigate;
mod json;
mod persisted_address;
mod persisted_review;
pub mod review;
mod util;

pub use address::AddressRequest;
pub use config::AppConfig;
pub use investigate::InvestigationRequest;
pub use review::ReviewRequest;

use std::{
    env,
    path::{Path, PathBuf},
    process::ExitCode,
};

#[derive(Debug, Clone)]
pub struct Anne {
    repo_root: PathBuf,
    config: AppConfig,
}

impl Anne {
    pub fn discover() -> Result<Self, String> {
        let cwd =
            env::current_dir().map_err(|error| format!("failed to read current dir: {error}"))?;
        Self::discover_from(&cwd)
    }

    pub fn discover_from(cwd: &Path) -> Result<Self, String> {
        let repo_root = git::repo_root(cwd)?;
        Self::open(&repo_root)
    }

    pub fn open(repo_root: &Path) -> Result<Self, String> {
        let config = AppConfig::load(repo_root)?;
        Ok(Self::from_parts(repo_root.to_path_buf(), config))
    }

    pub fn from_parts(repo_root: PathBuf, config: AppConfig) -> Self {
        Self { repo_root, config }
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn review(&self, request: review::ReviewRequest) -> Result<review::RunResult, String> {
        review::run_with(&self.repo_root, &self.config, request)
    }

    pub fn investigate(
        &self,
        request: investigate::InvestigationRequest,
    ) -> Result<investigate::RunResult, String> {
        investigate::run_with(&self.repo_root, &self.config, request)
    }

    pub fn address(&self, request: address::AddressRequest) -> Result<address::RunResult, String> {
        address::run_with(&self.repo_root, &self.config, request)
    }
}

pub fn run_from_env() -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    run_cli(&args)
}

pub fn run_cli(args: &[String]) -> ExitCode {
    match cli::parse(&args) {
        Ok(cli::Command::Help(text)) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Ok(cli::Command::Review(request)) => {
            match Anne::discover().and_then(|anne| anne.review(request)) {
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
            }
        }
        Ok(cli::Command::Investigate(request)) => {
            match Anne::discover().and_then(|anne| anne.investigate(request)) {
                Ok(result) => {
                    if let Some(bundle_path) = &result.bundle_path {
                        println!("Investigation bundle: {bundle_path}");
                        if result.success {
                            println!("Report: report.md");
                        }
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
            }
        }
        Ok(cli::Command::Address(request)) => {
            match Anne::discover().and_then(|anne| anne.address(request)) {
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
            }
        }
        Ok(cli::Command::Filter(request)) => match filter::run(request.target) {
            Ok(filter::RunResult::Comments {
                bundle_path,
                comments_loaded,
                comments_deleted,
                comments_remaining,
                completed,
            }) => {
                println!("Review bundle: {bundle_path}");
                println!("Comments loaded: {comments_loaded}");
                println!("Comments deleted: {comments_deleted}");
                println!("Comments remaining: {comments_remaining}");
                println!(
                    "Filter status: {}",
                    if completed { "completed" } else { "quit" }
                );
                ExitCode::SUCCESS
            }
            Ok(filter::RunResult::Specs {
                bundle_path,
                specs_loaded,
                specs_viewed,
                specs_remaining,
                completed,
            }) => {
                println!("Address bundle: {bundle_path}");
                println!("Specs loaded: {specs_loaded}");
                println!("Specs viewed: {specs_viewed}");
                println!("Specs remaining: {specs_remaining}");
                println!(
                    "Filter status: {}",
                    if completed { "completed" } else { "quit" }
                );
                ExitCode::SUCCESS
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
    use crate::{
        address::AddressRequest,
        cli::{self, Command, FilterRequest},
        filter::FilterTarget,
        investigate::InvestigationRequest,
        review::ReviewRequest,
    };

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
    fn parses_investigate_multi_word_prompt() {
        let command = cli::parse(&[
            "investigate".to_string(),
            "my".to_string(),
            "requests".to_string(),
            "keep".to_string(),
            "hanging".to_string(),
        ])
        .unwrap();

        assert_eq!(
            command,
            Command::Investigate(InvestigationRequest {
                prompt: "my requests keep hanging".to_string(),
            })
        );
    }

    #[test]
    fn investigate_without_prompt_returns_help() {
        let command = cli::parse(&["investigate".to_string()]).unwrap();

        match command {
            Command::Help(text) => assert!(text.contains("anne investigate")),
            other => panic!("expected help, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_investigate_flag() {
        let error =
            cli::parse(&["investigate".to_string(), "--prompt-file".to_string()]).unwrap_err();

        assert!(error.contains("unknown flag `--prompt-file`"));
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

    #[test]
    fn parses_filter_without_args() {
        let command = cli::parse(&["filter".to_string()]).unwrap();
        assert_eq!(
            command,
            Command::Filter(FilterRequest {
                target: FilterTarget::Comments
            })
        );
    }

    #[test]
    fn parses_filter_specs_target() {
        let command = cli::parse(&["filter".to_string(), "specs".to_string()]).unwrap();
        assert_eq!(
            command,
            Command::Filter(FilterRequest {
                target: FilterTarget::Specs
            })
        );
    }

    #[test]
    fn rejects_unknown_filter_target() {
        let error = cli::parse(&["filter".to_string(), "R001".to_string()]).unwrap_err();
        assert!(error.contains("unknown filter target"));
    }
}
