id = "template.stage.draft"
version = "v2"

cli = {
  positional = ["spec_file", "slug", "branch"]
  named = {
    file = "spec_file"
    name = "slug"
  }
}

params = {
  branch = ""
  commit_message = "chore: workflow stage commit"
  slug = ""
  spec_file = ""
  spec_source = "file"
  spec_text = ""
}

policy = {
  dependencies = {
    missing_producer = "wait"
  }
}

artifact_contracts = [
  { id = "prompt_text", version = "v1" },
  { id = "plan_text", version = "v1" },
  { id = "plan_branch", version = "v1" },
  { id = "plan_doc", version = "v1" }
]

nodes = [
  {
    id = "worktree_prepare"
    name = "Draft / Prepare Worktree"
    kind = "builtin"
    uses = "cap.env.builtin.worktree.prepare"
    args = {
      branch = "$${branch}"
      slug = "$${slug}"
      purpose = "stage-draft"
    }
    on = {
      succeeded = ["validate_provenance"]
    }
  },
  {
    id = "validate_provenance"
    name = "Draft / Validate Provenance"
    kind = "shell"
    uses = "cap.env.shell.command.run"
    args = {
      script = <<-SCRIPT
set -euo pipefail

spec_file="$${spec_file}"
spec_source="$(printf '%s' "$${spec_source}" | tr '[:upper:]' '[:lower:]')"

case "$spec_source" in
  file)
    if [ -z "$spec_file" ]; then
      printf 'draft provenance mismatch: spec_source=file requires spec_file\\n' >&2
      exit 1
    fi
    SPEC_FILE="$spec_file" python3 - <<'PY'
from pathlib import Path
import os
import sys

root = Path.cwd().resolve()
spec_file = os.environ["SPEC_FILE"]
spec_path = (root / spec_file).resolve()
if not spec_path.exists():
    sys.stderr.write(
        f"draft provenance mismatch: spec_file `{spec_file}` does not exist\\n"
    )
    raise SystemExit(1)
try:
    spec_path.relative_to(root)
except ValueError:
    sys.stderr.write(
        f"draft provenance mismatch: spec_file `{spec_file}` is outside the repository root\\n"
    )
    raise SystemExit(1)
PY
    ;;
  inline|stdin)
    if [ -n "$spec_file" ]; then
      printf 'draft provenance mismatch: spec_file=%s requires spec_source=file\\n' "$spec_file" >&2
      exit 1
    fi
    ;;
  *)
    printf 'draft provenance mismatch: unsupported spec_source `%s`\\n' "$spec_source" >&2
    exit 1
    ;;
esac
SCRIPT
    }
    on = {
      succeeded = ["resolve_prompt"]
    }
    after = [{ node_id = "worktree_prepare" }]
  },
  {
    id = "resolve_prompt"
    name = "Draft / Resolve Prompt"
    kind = "builtin"
    uses = "cap.env.builtin.prompt.resolve"
    args = {
      script = <<-SCRIPT
set -euo pipefail

spec_file="$${spec_file}"
SPEC_FILE="$spec_file" python3 - <<'PY'
from pathlib import Path
import os
import sys

template = Path(".vizier/prompts/DRAFT_PROMPTS.md").read_text()
spec_file = os.environ.get("SPEC_FILE", "").strip()

if spec_file:
    marker = "{{persist_plan.spec_text}}"
    if marker not in template:
        sys.stderr.write(
            "draft prompt template is missing {{persist_plan.spec_text}}\\n"
        )
        raise SystemExit(1)
    spec_text = Path(spec_file).read_text()
    sys.stdout.write(template.replace(marker, spec_text, 1))
else:
    sys.stdout.write(template)
PY
SCRIPT
    }
    produces = {
      succeeded = [{ custom = { type_id = "prompt_text", key = "draft_main" } }]
    }
    on = {
      succeeded = ["invoke_agent"]
    }
    after = [{ node_id = "validate_provenance" }]
  },
  {
    id = "invoke_agent"
    name = "Draft / Invoke Agent"
    kind = "agent"
    uses = "cap.agent.invoke"
    needs = [{ custom = { type_id = "prompt_text", key = "draft_main" } }]
    produces = {
      succeeded = [{ custom = { type_id = "plan_text", key = "draft_plan:$${slug}" } }]
    }
    on = {
      succeeded = ["persist_plan"]
    }
    after = [{ node_id = "resolve_prompt" }]
  },
  {
    id = "persist_plan"
    name = "Draft / Persist Plan"
    kind = "builtin"
    uses = "cap.env.builtin.plan.persist"
    args = {
      branch = "$${branch}"
      name_override = "$${slug}"
      spec_file = "$${spec_file}"
      spec_source = "$${spec_source}"
      spec_text = "$${spec_text}"
    }
    needs = [{ custom = { type_id = "plan_text", key = "draft_plan:$${slug}" } }]
    produces = {
      succeeded = [
        { plan_branch = { slug = "$${slug}", branch = "$${branch}" } },
        { plan_doc = { slug = "$${slug}", branch = "$${branch}" } }
      ]
    }
    on = {
      succeeded = ["rewrite_plan_state_provenance"]
    }
    after = [{ node_id = "invoke_agent" }]
  },
  {
    id = "rewrite_plan_state_provenance"
    name = "Draft / Rewrite Plan State Provenance"
    kind = "shell"
    uses = "cap.env.shell.command.run"
    args = {
      script = <<-SCRIPT
set -euo pipefail

slug="$${slug}"
spec_file="$${spec_file}"
spec_source="$${spec_source}"

SLUG="$slug" SPEC_FILE="$spec_file" SPEC_SOURCE="$spec_source" python3 - <<'PY'
from pathlib import Path
import json
import os
import sys

root = Path.cwd().resolve()
slug = os.environ["SLUG"].strip()
spec_file = os.environ.get("SPEC_FILE", "").strip()
spec_source = os.environ["SPEC_SOURCE"].strip().lower()

plan_doc = root / ".vizier" / "implementation-plans" / f"{slug}.md"
if not plan_doc.exists():
    sys.stderr.write(
        f"draft provenance rewrite failed: missing plan doc `{plan_doc.relative_to(root)}`\\n"
    )
    raise SystemExit(1)

plan_id = None
frontmatter_open = False
for line in plan_doc.read_text().splitlines():
    if line.strip() == "---":
        if frontmatter_open:
            break
        frontmatter_open = True
        continue
    if frontmatter_open and line.startswith("plan_id:"):
        plan_id = line.split(":", 1)[1].strip()
        break

if not plan_id:
    sys.stderr.write(
        f"draft provenance rewrite failed: missing plan_id front matter in `{plan_doc.relative_to(root)}`\\n"
    )
    raise SystemExit(1)

state_path = root / ".vizier" / "state" / "plans" / f"{plan_id}.json"
if not state_path.exists():
    sys.stderr.write(
        f"draft provenance rewrite failed: missing plan state `{state_path.relative_to(root)}`\\n"
    )
    raise SystemExit(1)

record = json.loads(state_path.read_text())

if spec_file:
    if spec_source != "file":
        sys.stderr.write(
            f"draft provenance rewrite failed: spec_file `{spec_file}` requires spec_source=file, got `{spec_source}`\\n"
        )
        raise SystemExit(1)
    spec_path = (root / spec_file).resolve()
    try:
        source_path = spec_path.relative_to(root).as_posix()
    except ValueError:
        sys.stderr.write(
            f"draft provenance rewrite failed: spec_file `{spec_file}` is outside the repository root\\n"
        )
        raise SystemExit(1)
    record["source"] = "file"
    record["source_path"] = source_path
else:
    if spec_source not in {"inline", "stdin"}:
        sys.stderr.write(
            f"draft provenance rewrite failed: inline plan state requires spec_source=inline|stdin, got `{spec_source}`\\n"
        )
        raise SystemExit(1)
    record["source"] = "inline"
    record.pop("source_path", None)

state_path.write_text(json.dumps(record, indent=2) + "\\n")
PY
SCRIPT
    }
    on = {
      succeeded = ["stage_files"]
    }
    after = [{ node_id = "persist_plan" }]
  },
  {
    id = "stage_files"
    name = "Draft / Stage Files"
    kind = "builtin"
    uses = "cap.env.builtin.git.stage"
    args = {
      files_json = "[\".\"]"
    }
    produces = {
      succeeded = [
        { plan_branch = { slug = "$${slug}", branch = "$${branch}" } },
        { plan_doc = { slug = "$${slug}", branch = "$${branch}" } }
      ]
    }
    after = [{ node_id = "rewrite_plan_state_provenance" }]
    on = {
      succeeded = ["stage_commit"]
    }
  },
  {
    id = "stage_commit"
    name = "Draft / Stage Commit"
    kind = "builtin"
    uses = "cap.env.builtin.git.commit"
    args = {
      message = "$${commit_message}"
    }
    produces = {
      succeeded = [
        { plan_branch = { slug = "$${slug}", branch = "$${branch}" } },
        { plan_doc = { slug = "$${slug}", branch = "$${branch}" } }
      ]
    }
    after = [{ node_id = "stage_files" }]
    on = {
      succeeded = ["stop_gate"]
    }
  },
  {
    id = "stop_gate"
    name = "Draft / Stop Gate"
    kind = "gate"
    uses = "control.gate.stop_condition"
    on = {
      succeeded = ["worktree_cleanup"]
    }
    after = [{ node_id = "stage_commit" }]
  },
  {
    id = "worktree_cleanup"
    name = "Draft / Cleanup Worktree"
    kind = "builtin"
    uses = "cap.env.builtin.worktree.cleanup"
    on = {
      succeeded = ["terminal"]
    }
    after = [{ node_id = "stop_gate" }]
  },
  {
    id = "terminal"
    name = "Draft / Terminal"
    kind = "gate"
    uses = "control.terminal"
    after = [{ node_id = "worktree_cleanup" }]
  }
]
