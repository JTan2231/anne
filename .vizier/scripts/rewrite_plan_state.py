#!/usr/bin/env python3

from __future__ import annotations

from pathlib import Path
import argparse
import json
import re
import sys

SUMMARY_LIMIT = 120
GENERIC_HEADINGS = {
    "anne",
    "operator spec",
    "implementation plan",
    "source comment",
    "overview",
    "execution plan",
    "risks & unknowns",
    "testing & verification",
    "notes",
    "assumptions and open questions",
    "problem",
    "goals",
    "non-goals",
    "proposed approach",
}
IGNORED_SPEC_SECTIONS = {"source comment"}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--slug", required=True)
    parser.add_argument("--spec-file", default="")
    parser.add_argument("--spec-source", required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()

    root = Path.cwd().resolve()
    slug = args.slug.strip()
    spec_file = args.spec_file.strip()
    spec_source = args.spec_source.strip().lower()

    plan_doc = root / ".vizier" / "implementation-plans" / f"{slug}.md"
    if not plan_doc.exists():
        fail(
            root,
            plan_doc,
            f"draft plan-state rewrite failed: missing plan doc `{plan_doc.relative_to(root)}`",
        )

    plan_doc_text = plan_doc.read_text()
    plan_id = extract_plan_id(plan_doc_text)
    if not plan_id:
        fail(
            root,
            plan_doc,
            (
                "draft plan-state rewrite failed: missing plan_id front matter in "
                f"`{plan_doc.relative_to(root)}`"
            ),
        )

    state_path = root / ".vizier" / "state" / "plans" / f"{plan_id}.json"
    if not state_path.exists():
        fail(
            root,
            state_path,
            f"draft plan-state rewrite failed: missing plan state `{state_path.relative_to(root)}`",
        )

    record = json.loads(state_path.read_text())
    record["summary"] = derive_summary(slug, plan_doc_text)

    if spec_file:
        if spec_source != "file":
            sys.stderr.write(
                "draft plan-state rewrite failed: "
                f"spec_file `{spec_file}` requires spec_source=file, got `{spec_source}`\n"
            )
            return 1
        spec_path = (root / spec_file).resolve()
        try:
            source_path = spec_path.relative_to(root).as_posix()
        except ValueError:
            sys.stderr.write(
                "draft plan-state rewrite failed: "
                f"spec_file `{spec_file}` is outside the repository root\n"
            )
            return 1
        record["source"] = "file"
        record["source_path"] = source_path
    else:
        if spec_source not in {"inline", "stdin"}:
            sys.stderr.write(
                "draft plan-state rewrite failed: "
                "inline plan state requires spec_source=inline|stdin, "
                f"got `{spec_source}`\n"
            )
            return 1
        record["source"] = "inline"
        record.pop("source_path", None)

    state_path.write_text(json.dumps(record, indent=2) + "\n")
    return 0


def fail(root: Path, path: Path, message: str) -> None:
    del root, path
    sys.stderr.write(f"{message}\n")
    raise SystemExit(1)


def extract_plan_id(plan_doc_text: str) -> str:
    if not plan_doc_text.startswith("---\n"):
        return ""

    frontmatter_lines = []
    for line in plan_doc_text.splitlines()[1:]:
        if line.strip() == "---":
            break
        frontmatter_lines.append(line)

    for line in frontmatter_lines:
        if line.startswith("plan_id:"):
            return line.split(":", 1)[1].strip()
    return ""


def derive_summary(slug: str, plan_doc_text: str) -> str:
    plan_text = strip_frontmatter(plan_doc_text)
    operator_spec = extract_top_level_section(
        plan_text,
        start_heading="## Operator Spec",
        end_headings={"## Implementation Plan"},
    )
    implementation_plan = extract_top_level_section(
        plan_text,
        start_heading="## Implementation Plan",
    )

    candidates = [
        first_overview_prose_line(implementation_plan),
        first_non_heading_line(implementation_plan),
        first_specific_heading(operator_spec),
        first_non_heading_line(operator_spec, ignored_sections=IGNORED_SPEC_SECTIONS),
        normalize_summary(f"Plan {slug}"),
    ]

    for candidate in candidates:
        if candidate:
            return candidate
    return f"Plan {slug}"


def strip_frontmatter(plan_doc_text: str) -> str:
    if not plan_doc_text.startswith("---\n"):
        return plan_doc_text

    lines = plan_doc_text.splitlines()
    for index, line in enumerate(lines[1:], start=1):
        if line.strip() == "---":
            return "\n".join(lines[index + 1 :])
    return plan_doc_text


def extract_top_level_section(
    markdown: str,
    start_heading: str,
    end_headings: set[str] | None = None,
) -> str:
    lines = markdown.splitlines()
    capture = False
    collected: list[str] = []
    end_headings = end_headings or set()

    for line in lines:
        stripped = line.strip()
        if stripped == start_heading:
            capture = True
            continue
        if capture and stripped in end_headings:
            break
        if capture:
            collected.append(line)

    return "\n".join(collected)


def extract_section(markdown: str, heading: str) -> str:
    lines = markdown.splitlines()
    target = heading.strip().lower()
    capture = False
    collected: list[str] = []

    for line in lines:
        stripped = line.strip()
        if stripped.startswith("## "):
            current = stripped[3:].strip().lower()
            if capture and current != target:
                break
            if current == target:
                capture = True
                continue
        if capture:
            collected.append(line)

    return "\n".join(collected)


def first_overview_prose_line(markdown: str) -> str:
    overview = extract_section(markdown, "Overview")
    if not overview:
        return ""

    for line in overview.splitlines():
        stripped = line.strip()
        if not stripped or is_heading(stripped) or is_list_item(stripped):
            continue
        candidate = normalize_summary(stripped)
        if candidate:
            return candidate
    return ""


def first_non_heading_line(markdown: str, ignored_sections: set[str] | None = None) -> str:
    ignored_sections = ignored_sections or set()
    current_section = ""

    for line in markdown.splitlines():
        stripped = line.strip()
        if stripped.startswith("## "):
            current_section = normalize_summary(stripped).lower()
            continue
        if not stripped or is_heading(stripped):
            continue
        if current_section in ignored_sections:
            continue
        candidate = normalize_summary(stripped)
        if candidate:
            return candidate
    return ""


def first_specific_heading(markdown: str) -> str:
    for line in markdown.splitlines():
        stripped = line.strip()
        if not is_heading(stripped):
            continue
        candidate = normalize_summary(stripped)
        if candidate and candidate.lower() not in GENERIC_HEADINGS:
            return candidate
    return ""


def is_heading(text: str) -> bool:
    return text.startswith("#")


def is_list_item(text: str) -> bool:
    return bool(re.match(r"^([-+*]|\d+\.)\s+", text))


def normalize_summary(text: str) -> str:
    text = re.sub(r"\[([^\]]+)\]\([^)]+\)", r"\1", text)
    text = re.sub(r"`([^`]+)`", r"\1", text)
    text = re.sub(r"^\s{0,3}#{1,6}\s*", "", text)
    text = re.sub(r"^\s{0,3}[-+*]\s+", "", text)
    text = re.sub(r"^\s{0,3}\d+\.\s+", "", text)
    text = re.sub(r"^\s{0,3}>\s*", "", text)
    text = re.sub(r"\s+", " ", text).strip()

    if len(text) <= SUMMARY_LIMIT:
        return text
    return text[: SUMMARY_LIMIT - 3].rstrip() + "..."


if __name__ == "__main__":
    raise SystemExit(main())
