"""Bind a read-only PR build to a merged tree, then publish its bytes once.

The publisher never imports or executes downloaded code. All external commands
use argument arrays; artifact paths, targets and hashes are validated as data.
"""

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile
import tomllib
import zipfile


TARGETS = (
    "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
)
WORKFLOW = ".github/workflows/release.yml"
MAX_ARCHIVE = 384 * 1024 * 1024


def require(condition, message):
    if not condition:
        raise ValueError(message)


def command(*args):
    result = subprocess.run(args, capture_output=True, check=False)
    if result.returncode:
        raise RuntimeError(f"{args[0]} failed: {result.stderr.decode(errors='replace')}")
    return result.stdout


def git(*args):
    return command("git", *args).decode().strip()


def api(endpoint, optional=False):
    result = subprocess.run(["gh", "api", endpoint], capture_output=True, check=False)
    if optional and result.returncode and b"HTTP 404" in result.stderr:
        return None
    require(result.returncode == 0, f"GitHub read failed: {result.stderr.decode(errors='replace')}")
    return json.loads(result.stdout)


def listing(endpoint, key=None):
    """Bounded pagination; never silently approve an incomplete listing."""
    items = []
    for page in range(1, 11):
        separator = "&" if "?" in endpoint else "?"
        value = api(f"{endpoint}{separator}per_page=100&page={page}")
        batch = value[key] if key else value
        items.extend(batch)
        if len(batch) < 100:
            return items
    raise ValueError("GitHub listing exceeds the 1,000-item verification bound")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def version_of(content):
    version = tomllib.loads(content)["workspace"]["package"]["version"]
    require(re.fullmatch(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", version),
            "Release version must be a full stable semver")
    return version


def version_tuple(version):
    return tuple(map(int, version.split(".")))


def archive_name(target):
    require(target in TARGETS, "Unexpected release target")
    suffix = ".zip" if target.endswith("windows-msvc") else ".tar.gz"
    return f"agentlaw-{target}{suffix}"


def manifest(target, directory):
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    require(os.environ["GITHUB_EVENT_NAME"] == "pull_request", "Candidate must come from a PR")
    pr = event["pull_request"]
    archive = Path(directory) / archive_name(target)
    contents = archive.read_bytes()
    require(0 < len(contents) <= MAX_ARCHIVE, "Candidate archive size out of bounds")
    value = {
        "schema": 1,
        "repository": os.environ["GITHUB_REPOSITORY"],
        "run_id": int(os.environ["GITHUB_RUN_ID"]),
        "run_attempt": int(os.environ["GITHUB_RUN_ATTEMPT"]),
        "pull_request": pr["number"],
        "head_sha": pr["head"]["sha"],
        "base_sha": pr["base"]["sha"],
        "tested_commit": git("rev-parse", "HEAD"),
        "tree": git("rev-parse", "HEAD^{tree}"),
        "version": version_of(Path("Cargo.toml").read_text()),
        "target": target,
        "archive": archive.name,
        "bytes": len(contents),
        "sha256": digest(contents),
    }
    require(value["tested_commit"] == os.environ["GITHUB_SHA"], "Checkout is not the event's tested merge")
    (Path(directory) / "candidate.json").write_text(json.dumps(value, sort_keys=True) + "\n")


def validate_pr(pr, repository, commit):
    require(pr["merged"] and pr["merge_commit_sha"] == commit, "PR does not own this merged commit")
    require(pr["base"]["ref"] == "main" and pr["base"]["repo"]["full_name"] == repository,
            "PR base is not this repository's main")
    require(pr["head"]["repo"] and pr["head"]["repo"]["full_name"] == repository,
            "Automatic release requires a same-repository PR")


def select_run(runs, repository, head):
    matches = [run for run in runs if (
        run["event"] == "pull_request" and run["status"] == "completed"
        and run["conclusion"] == "success" and run["head_sha"] == head
        and run["path"] == WORKFLOW
        and run["repository"]["full_name"] == repository
        and run["head_repository"]["full_name"] == repository
    )]
    require(matches, "No successful PR candidate run for the exact reviewed head")
    return max(matches, key=lambda run: run["id"])


def select_artifacts(artifacts, run, repository_id):
    selected = {}
    for target in TARGETS:
        name = f"agentlaw-{target}-{run['id']}-{run['run_attempt']}"
        matches = [item for item in artifacts if item["name"] == name]
        require(len(matches) == 1, f"Expected one current-attempt artifact for {target}")
        artifact = matches[0]
        origin = artifact["workflow_run"]
        require(not artifact["expired"], "Candidate expired; never silently rebuild at publication")
        require(origin["id"] == run["id"] and origin["head_sha"] == run["head_sha"]
                and origin["repository_id"] == repository_id
                and origin["head_repository_id"] == repository_id, "Artifact origin mismatch")
        require(re.fullmatch(r"sha256:[0-9a-f]{64}", artifact.get("digest") or ""),
                "Artifact has no verifiable GitHub SHA-256 digest")
        require(0 < artifact["size_in_bytes"] <= MAX_ARCHIVE, "Artifact size out of bounds")
        selected[target] = artifact
    return selected


def unpack_candidate(data, artifact, expected, target):
    require(0 < len(data) <= MAX_ARCHIVE, "Downloaded artifact exceeds bound")
    require("sha256:" + digest(data) == artifact["digest"], "GitHub artifact digest mismatch")
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        entries = archive.infolist()
        names = [entry.filename for entry in entries]
        wanted = {"candidate.json", archive_name(target)}
        require(len(entries) == 2 and set(names) == wanted, "Unexpected or duplicate artifact member")
        for entry in entries:
            kind = stat.S_IFMT(entry.external_attr >> 16)
            require(not entry.is_dir() and kind in (0, stat.S_IFREG), "Non-regular artifact member")
            limit = 65536 if entry.filename == "candidate.json" else MAX_ARCHIVE
            require(0 < entry.file_size <= limit, "Artifact member size out of bounds")
        candidate = json.loads(archive.read("candidate.json"))
        for key, value in expected.items():
            require(candidate.get(key) == value, f"Candidate {key} does not match merged release")
        require(candidate.get("target") == target and candidate.get("archive") == archive_name(target),
                "Candidate target mismatch")
        require(re.fullmatch(r"[0-9a-f]{40}", candidate.get("tested_commit", "")), "Invalid tested commit")
        payload = archive.read(archive_name(target))
        require(len(payload) == candidate.get("bytes") and digest(payload) == candidate.get("sha256"),
                "Candidate payload digest/size mismatch")
        return candidate, payload


def verify_jobs(jobs):
    for target in TARGETS:
        matches = [job for job in jobs if job["name"] == f"Test and package ({target})"]
        require(len(matches) == 1 and matches[0]["conclusion"] == "success", "Target job not successful")
        steps = {step["name"]: step["conclusion"] for step in matches[0]["steps"]}
        for name in ("Check release helper", "Format and test", "Build release executables", "Package"):
            require(steps.get(name) == "success", f"Required CI step did not succeed: {name}")


def verify_release_assets(assets, files, complete):
    names = [asset["name"] for asset in assets]
    require(len(set(names)) == len(names) and set(names) <= set(files), "Unexpected release assets")
    if complete:
        require(set(names) == set(files), "Published asset set is incomplete")
    for asset in assets:
        content = files[asset["name"]].read_bytes()
        require(asset["state"] == "uploaded" and asset["size"] == len(content)
                and asset.get("digest") == "sha256:" + digest(content),
                f"Existing release asset differs: {asset['name']}")


def find_release(prefix, tag):
    # The tag endpoint only finds published releases; drafts need the list API.
    matches = [item for item in listing(f"{prefix}/releases") if item["tag_name"] == tag]
    require(len(matches) <= 1, "Multiple releases name the same version")
    return matches[0] if matches else None


def publish_files(repository, commit, version, files, notes):
    prefix = f"repos/{repository}"
    tag = "v" + version
    release = find_release(prefix, tag)
    ref = api(f"{prefix}/git/ref/tags/{tag}", optional=True)
    if ref:
        obj = ref["object"]
        for _ in range(4):
            if obj["type"] == "commit":
                break
            require(obj["type"] == "tag", "Unsupported release tag object")
            obj = api(f"{prefix}/git/tags/{obj['sha']}")["object"]
        require(obj["type"] == "commit" and obj["sha"] == commit, "Existing version tag names another commit")
    require(not release or ref, "Existing release has no verified tag")
    if release:
        require(not release["prerelease"], "Existing version is a prerelease")
        verify_release_assets(release["assets"], files, complete=not release["draft"])
        if not release["draft"]:
            print(f"Already published and verified: {release['html_url']}")
            return
    latest = api(f"{prefix}/releases/latest", optional=True)
    if latest:
        latest_version = latest["tag_name"].removeprefix("v")
        require(re.fullmatch(r"\d+\.\d+\.\d+", latest_version), "Latest release has an unsupported version")
        require(version_tuple(version) > version_tuple(latest_version), "Refusing to replace latest with an older version")
    if not ref:
        command("gh", "api", f"{prefix}/git/refs", "--method", "POST",
                "-f", f"ref=refs/tags/{tag}", "-f", f"sha={commit}")
    if not release:
        command("gh", "release", "create", tag, "--repo", repository, "--verify-tag", "--draft",
                "--title", f"Agentlaw {version}", "--notes-file", str(notes))
        release = find_release(prefix, tag)
        require(release is not None, "Created release is not visible; retry publication")
    existing = {asset["name"] for asset in release["assets"]}
    for name in sorted(files):
        if name not in existing:
            command("gh", "release", "upload", tag, str(files[name]), "--repo", repository)
    release = api(f"{prefix}/releases/{release['id']}")
    verify_release_assets(release["assets"], files, complete=True)
    command("gh", "release", "edit", tag, "--repo", repository, "--draft=false", "--latest")
    release = api(f"{prefix}/releases/{release['id']}")
    require(not release["draft"], "Release is still draft")
    verify_release_assets(release["assets"], files, complete=True)
    print(f"Published existing verified artifacts: {release['html_url']}")


def publish():
    repository = os.environ["GITHUB_REPOSITORY"]
    require(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository), "Invalid repository")
    require(os.environ["GITHUB_REF"] == "refs/heads/main", "Publisher must execute from main")
    commit = os.environ.get("RELEASE_COMMIT") or os.environ["GITHUB_SHA"]
    require(re.fullmatch(r"[0-9a-f]{40}", commit), "Expected full merged commit SHA")
    git("merge-base", "--is-ancestor", commit, "origin/main")
    parents = git("rev-list", "--parents", "-n", "1", commit).split()[1:]
    require(parents, "Release commit has no previous main state")
    version = version_of(git("show", f"{commit}:Cargo.toml"))
    previous = version_of(git("show", f"{parents[0]}:Cargo.toml"))
    if version == previous:
        print("No version change; no release requested by this merge.")
        return
    require(version_tuple(version) > version_tuple(previous), "Version must increase")
    prefix = f"repos/{repository}"
    linked = listing(f"{prefix}/commits/{commit}/pulls")
    matching = [pr for pr in linked if pr.get("merge_commit_sha") == commit and pr["base"]["ref"] == "main"]
    require(len(matching) == 1, "Release must identify exactly one merged PR")
    pr = api(f"{prefix}/pulls/{matching[0]['number']}")
    validate_pr(pr, repository, commit)
    runs = listing(f"{prefix}/actions/workflows/release.yml/runs?event=pull_request&head_sha={pr['head']['sha']}", "workflow_runs")
    run = select_run(runs, repository, pr["head"]["sha"])
    jobs = listing(f"{prefix}/actions/runs/{run['id']}/attempts/{run['run_attempt']}/jobs", "jobs")
    verify_jobs(jobs)
    artifacts = select_artifacts(listing(f"{prefix}/actions/runs/{run['id']}/artifacts", "artifacts"),
                                 run, pr["base"]["repo"]["id"])
    expected = {
        "schema": 1, "repository": repository, "run_id": run["id"], "run_attempt": run["run_attempt"],
        "pull_request": pr["number"], "head_sha": pr["head"]["sha"],
        "tree": git("rev-parse", f"{commit}^{{tree}}"), "version": version,
    }
    with tempfile.TemporaryDirectory(prefix="agentlaw-release-") as directory:
        destination = Path(directory)
        files = {}
        proof = {"version": version, "merged_commit": commit, "tree": expected["tree"],
                 "candidate_run": run["id"], "candidate_attempt": run["run_attempt"], "artifacts": []}
        tested = set()
        for target, artifact in artifacts.items():
            data = command("gh", "api", f"{prefix}/actions/artifacts/{artifact['id']}/zip")
            candidate, payload = unpack_candidate(data, artifact, expected, target)
            key = (candidate["tested_commit"], candidate["base_sha"])
            if key not in tested:
                source = api(f"{prefix}/git/commits/{candidate['tested_commit']}")
                require(source["tree"]["sha"] == expected["tree"], "Tested Git tree differs from merged source")
                require([item["sha"] for item in source["parents"]] == [candidate["base_sha"], pr["head"]["sha"]],
                        "Candidate is not the PR's synthetic merge")
                tested.add(key)
            name = archive_name(target)
            files[name] = destination / name
            files[name].write_bytes(payload)
            proof["artifacts"].append({"target": target, "id": artifact["id"], "digest": artifact["digest"],
                                       "archive": name, "sha256": candidate["sha256"]})
        require(len(tested) == 1, "Targets were built from different merge candidates")
        for name in ("install.sh", "install.ps1"):
            files[name] = destination / name
            files[name].write_bytes(command("git", "show", f"{commit}:{name}"))
        files["release-provenance.json"] = destination / "release-provenance.json"
        files["release-provenance.json"].write_text(json.dumps(proof, sort_keys=True, indent=2) + "\n")
        checksums = "".join(f"{digest(files[name].read_bytes())}  {name}\n" for name in sorted(files))
        files["SHA256SUMS"] = destination / "SHA256SUMS"
        files["SHA256SUMS"].write_text(checksums)
        notes = destination / "release-notes.md"
        notes.write_bytes(command("git", "show", f"{commit}:docs/release-notes.md"))
        print(json.dumps(proof, sort_keys=True))
        publish_files(repository, commit, version, files, notes)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    candidate_parser = sub.add_parser("manifest")
    candidate_parser.add_argument("--target", required=True, choices=TARGETS)
    candidate_parser.add_argument("--directory", required=True)
    sub.add_parser("publish")
    args = parser.parse_args()
    if args.action == "manifest":
        manifest(args.target, args.directory)
    else:
        publish()
