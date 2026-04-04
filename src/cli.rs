#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRequest {
    pub base: String,
    pub head: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help(String),
    Review(ReviewRequest),
}

pub fn parse(args: &[String]) -> Result<Command, String> {
    if args.is_empty() {
        return Ok(Command::Help(root_help()));
    }

    match args[0].as_str() {
        "-h" | "--help" | "help" => Ok(Command::Help(root_help())),
        "review" => parse_review(&args[1..]),
        other => Err(format!("unknown command `{other}`\n\n{}", root_help())),
    }
}

fn parse_review(args: &[String]) -> Result<Command, String> {
    if args.is_empty() {
        return Ok(Command::Help(review_help()));
    }

    let mut base = None;
    let mut head = None;
    let mut triple_dot = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(Command::Help(review_help())),
            "--base" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--base requires a value".to_string())?;
                if base.replace(value.clone()).is_some() {
                    return Err("--base was provided more than once".to_string());
                }
                index += 2;
            }
            "--head" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--head requires a value".to_string())?;
                if head.replace(value.clone()).is_some() {
                    return Err("--head was provided more than once".to_string());
                }
                index += 2;
            }
            value if value.starts_with("--") => {
                return Err(format!("unknown flag `{value}`"));
            }
            value => {
                if triple_dot.replace(value.to_string()).is_some() {
                    return Err(
                        "review accepts only one positional <base>...<head> argument".to_string(),
                    );
                }
                index += 1;
            }
        }
    }

    if triple_dot.is_some() && (base.is_some() || head.is_some()) {
        return Err("provide either --base/--head or <base>...<head>, not both".to_string());
    }

    if let Some(range) = triple_dot {
        let Some((base, head)) = range.split_once("...") else {
            return Err(
                "review positional syntax must be <base>...<head> using triple-dot semantics"
                    .to_string(),
            );
        };

        if base.is_empty() || head.is_empty() || head.contains("...") {
            return Err(
                "review positional syntax must be exactly one <base>...<head> range".to_string(),
            );
        }

        return Ok(Command::Review(ReviewRequest {
            base: base.to_string(),
            head: head.to_string(),
        }));
    }

    match (base, head) {
        (Some(base), Some(head)) => Ok(Command::Review(ReviewRequest { base, head })),
        (Some(_), None) | (None, Some(_)) => {
            Err("review requires both --base and --head".to_string())
        }
        (None, None) => Ok(Command::Help(review_help())),
    }
}

fn root_help() -> String {
    format!(
        "{binary}\n\nCommands:\n  review   Review <base>...<head> using git merge-base semantics\n\nRun `{binary} review --help` for details.",
        binary = "anne"
    )
}

fn review_help() -> String {
    "\
anne review

Review a branch diff using merge-base semantics and write a local review bundle.

Usage:
  anne review --base <branch> --head <branch>
  anne review <base>...<head>

Notes:
  - Triple-dot review is canonical user-facing syntax.
  - Anne resolves `git merge-base <base> <head>` and reviews the diff from that merge base to `<head>`.
  - Two-dot range math is intentionally not accepted here."
        .to_string()
}
