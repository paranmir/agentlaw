import io
import json
from pathlib import Path
import stat
import tempfile
import unittest
from unittest.mock import patch
import warnings
import zipfile

import release


REPOSITORY = "owner/product"
HEAD = "a" * 40
MERGE = "b" * 40
TREE = "c" * 40
BASE = "d" * 40
TARGET = release.TARGETS[0]


def run_fixture():
    return {"id": 20, "run_attempt": 2, "event": "pull_request", "status": "completed",
            "conclusion": "success", "head_sha": HEAD, "path": release.WORKFLOW,
            "repository": {"full_name": REPOSITORY}, "head_repository": {"full_name": REPOSITORY}}


def pr_fixture():
    return {"number": 3, "merged": True, "merge_commit_sha": MERGE,
            "base": {"ref": "main", "repo": {"full_name": REPOSITORY}},
            "head": {"sha": HEAD, "repo": {"full_name": REPOSITORY}}}


def expected_fixture():
    return {"schema": 1, "repository": REPOSITORY, "run_id": 20, "run_attempt": 2,
            "pull_request": 3, "head_sha": HEAD, "tree": TREE, "version": "0.4.1"}


def candidate_zip(change=None, extra=None, symlink=False, target=TARGET):
    payload = b"opaque release archive bytes"
    candidate = {**expected_fixture(), "target": target, "archive": release.archive_name(target),
                 "tested_commit": MERGE, "base_sha": BASE, "bytes": len(payload),
                 "sha256": release.digest(payload)}
    candidate.update(change or {})
    out = io.BytesIO()
    with zipfile.ZipFile(out, "w") as archive:
        archive.writestr("candidate.json", json.dumps(candidate))
        info = zipfile.ZipInfo(release.archive_name(target))
        if symlink:
            info.create_system = 3
            info.external_attr = (stat.S_IFLNK | 0o777) << 16
        archive.writestr(info, payload)
        if extra:
            with warnings.catch_warnings():
                warnings.simplefilter("ignore", UserWarning)
                archive.writestr(extra, b"not admitted")
    data = out.getvalue()
    return data, {"digest": "sha256:" + release.digest(data)}


class ReleaseTests(unittest.TestCase):
    def test_stable_version_is_read_from_workspace_not_dependency(self):
        self.assertEqual(release.version_of('[workspace.package]\nversion="0.4.1"\n[dependencies]\nx="1"'), "0.4.1")
        for version in ("0.4.1-rc.1", "01.2.3", "0.4", "0.4.1+build"):
            with self.subTest(version=version), self.assertRaises(ValueError):
                release.version_of(f'[workspace.package]\nversion="{version}"')

    def test_pr_must_be_merged_same_repository_main_and_exact_commit(self):
        release.validate_pr(pr_fixture(), REPOSITORY, MERGE)
        for change in ("unmerged", "commit", "base", "fork", "missing_repo"):
            pr = pr_fixture()
            if change == "unmerged":
                pr["merged"] = False
            elif change == "commit":
                pr["merge_commit_sha"] = HEAD
            elif change == "base":
                pr["base"]["ref"] = "develop"
            elif change == "fork":
                pr["head"]["repo"]["full_name"] = "other/fork"
            else:
                pr["head"]["repo"] = None
            with self.subTest(change=change), self.assertRaises(ValueError):
                release.validate_pr(pr, REPOSITORY, MERGE)

    def test_run_requires_correct_event_workflow_repo_head_and_success(self):
        good = run_fixture()
        self.assertEqual(release.select_run([good], REPOSITORY, HEAD), good)
        for key, value in (("event", "push"), ("head_sha", MERGE), ("conclusion", "failure"),
                           ("status", "in_progress"), ("path", ".github/workflows/unrelated.yml"),
                           ("head_repository", {"full_name": "other/fork"})):
            bad = {**good, key: value}
            with self.subTest(key=key), self.assertRaises(ValueError):
                release.select_run([bad], REPOSITORY, HEAD)

    def test_latest_eligible_success_is_selected_not_unrelated_newer_run(self):
        good = run_fixture()
        newer = {**good, "id": 21}
        unrelated = {**good, "id": 22, "path": "other"}
        self.assertEqual(release.select_run([good, newer, unrelated], REPOSITORY, HEAD), newer)

    def artifacts(self):
        return [{"id": index, "name": f"agentlaw-{target}-20-2", "expired": False,
                 "digest": "sha256:" + "e" * 64, "size_in_bytes": 123,
                 "workflow_run": {"id": 20, "head_sha": HEAD, "repository_id": 5, "head_repository_id": 5}}
                for index, target in enumerate(release.TARGETS)]

    def test_all_four_current_attempt_artifacts_are_required(self):
        artifacts = self.artifacts()
        self.assertEqual(set(release.select_artifacts(artifacts, run_fixture(), 5)), set(release.TARGETS))
        for changed in (artifacts[:-1], artifacts + [artifacts[0]],
                        [{**artifacts[0], "name": artifacts[0]["name"].replace("-20-2", "-20-1")}, *artifacts[1:]]):
            with self.assertRaises(ValueError):
                release.select_artifacts(changed, run_fixture(), 5)

    def test_expiry_digest_origin_and_size_fail_closed(self):
        for key, value in (("expired", True), ("digest", None), ("size_in_bytes", 0),
                           ("size_in_bytes", release.MAX_ARCHIVE + 1),
                           ("workflow_run", {"id": 21, "head_sha": HEAD, "repository_id": 5, "head_repository_id": 5})):
            artifacts = self.artifacts()
            artifacts[0][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                release.select_artifacts(artifacts, run_fixture(), 5)

    def test_zip_bytes_and_inner_payload_are_both_verified(self):
        data, artifact = candidate_zip()
        value, payload = release.unpack_candidate(data, artifact, expected_fixture(), TARGET)
        self.assertEqual(value["sha256"], release.digest(payload))
        with self.assertRaises(ValueError):
            release.unpack_candidate(data + b"tamper", artifact, expected_fixture(), TARGET)
        for change in ({"sha256": "f" * 64}, {"bytes": 1}):
            data, artifact = candidate_zip(change)
            with self.assertRaises(ValueError):
                release.unpack_candidate(data, artifact, expected_fixture(), TARGET)

    def test_manifest_cannot_change_release_identity(self):
        for key, value in (("tree", BASE), ("version", "0.4.2"), ("run_attempt", 1),
                           ("run_id", 21), ("head_sha", MERGE), ("pull_request", 4),
                           ("repository", "other/repo"), ("target", release.TARGETS[1])):
            data, artifact = candidate_zip({key: value})
            with self.subTest(key=key), self.assertRaises(ValueError):
                release.unpack_candidate(data, artifact, expected_fixture(), TARGET)

    def test_zip_traversal_extra_members_and_symlinks_are_rejected(self):
        for extra in ("../outside", "candidate.json", "unexpected"):
            data, artifact = candidate_zip(extra=extra)
            with self.subTest(extra=extra), self.assertRaises(ValueError):
                release.unpack_candidate(data, artifact, expected_fixture(), TARGET)
        data, artifact = candidate_zip(symlink=True)
        with self.assertRaises(ValueError):
            release.unpack_candidate(data, artifact, expected_fixture(), TARGET)

    def jobs(self):
        return [{"name": f"Test and package ({target})", "conclusion": "success", "steps": [
            {"name": name, "conclusion": "success"} for name in (
                "Check release helper", "Format and test", "Build release executables", "Package")
        ]} for target in release.TARGETS]

    def test_successful_run_cannot_hide_skipped_build_or_test(self):
        release.verify_jobs(self.jobs())
        jobs = self.jobs()
        jobs[0]["steps"][2]["conclusion"] = "skipped"
        with self.assertRaises(ValueError):
            release.verify_jobs(jobs)
        with self.assertRaises(ValueError):
            release.verify_jobs(self.jobs()[:-1])

    def test_release_assets_must_be_exact_and_no_overwrite_is_allowed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "asset"
            path.write_bytes(b"data")
            asset = {"name": "asset", "state": "uploaded", "size": 4,
                     "digest": "sha256:" + release.digest(b"data")}
            release.verify_release_assets([asset], {"asset": path}, True)
            release.verify_release_assets([], {"asset": path}, False)
            for assets in ([], [asset, asset], [{**asset, "name": "extra"}],
                           [{**asset, "digest": "sha256:" + "a" * 64}]):
                with self.assertRaises(ValueError):
                    release.verify_release_assets(assets, {"asset": path}, True)

    def test_published_identical_release_is_read_only(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "asset"
            path.write_bytes(b"data")
            published = {"draft": False, "prerelease": False, "html_url": "release-url", "assets": [
                {"name": "asset", "state": "uploaded", "size": 4, "digest": "sha256:" + release.digest(b"data")}]}
            ref = {"object": {"type": "commit", "sha": MERGE}}
            with patch.object(release, "find_release", return_value=published), \
                    patch.object(release, "api", return_value=ref), patch.object(release, "command") as commands:
                release.publish_files(REPOSITORY, MERGE, "0.4.1", {"asset": path}, path)
                commands.assert_not_called()

    def test_existing_tag_for_other_commit_is_never_retargeted(self):
        with patch.object(release, "find_release", return_value=None), \
                patch.object(release, "api", return_value={"object": {"type": "commit", "sha": HEAD}}), \
                patch.object(release, "command") as commands:
            with self.assertRaises(ValueError):
                release.publish_files(REPOSITORY, MERGE, "0.4.1", {}, Path("notes"))
            commands.assert_not_called()

    def test_draft_resume_uploads_only_missing_assets_then_publishes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "asset"
            path.write_bytes(b"data")
            draft = {"id": 42, "draft": True, "prerelease": False, "html_url": "release-url", "assets": []}
            complete = {**draft, "assets": [{"name": "asset", "state": "uploaded", "size": 4,
                                            "digest": "sha256:" + release.digest(b"data")}]}
            responses = [{"object": {"type": "commit", "sha": MERGE}}, {"tag_name": "v0.4.0"},
                         complete, {**complete, "draft": False}]
            with patch.object(release, "find_release", return_value=draft), \
                    patch.object(release, "api", side_effect=responses) as reads, patch.object(release, "command") as commands:
                release.publish_files(REPOSITORY, MERGE, "0.4.1", {"asset": path}, path)
                self.assertEqual([call.args[2] for call in commands.call_args_list], ["upload", "edit"])
                self.assertNotIn("--clobber", str(commands.call_args_list))
                self.assertEqual(reads.call_args.args[0], f"repos/{REPOSITORY}/releases/42")

    def test_find_release_includes_drafts_and_rejects_duplicate_tags(self):
        draft = {"id": 42, "tag_name": "v0.4.1", "draft": True}
        with patch.object(release, "listing", return_value=[draft]) as listing:
            self.assertEqual(release.find_release("repos/owner/product", "v0.4.1"), draft)
            listing.assert_called_once_with("repos/owner/product/releases")
            self.assertIsNone(release.find_release("repos/owner/product", "v0.4.2"))
        with patch.object(release, "listing", return_value=[draft, draft]):
            with self.assertRaises(ValueError):
                release.find_release("repos/owner/product", "v0.4.1")

    def test_new_draft_is_found_by_list_and_refreshed_by_id(self):
        draft = {"id": 42, "draft": True, "prerelease": False, "assets": [], "html_url": "release-url"}
        with patch.object(release, "find_release", side_effect=[None, draft]), \
                patch.object(release, "api", side_effect=[None, None, draft, {**draft, "draft": False}]) as reads, \
                patch.object(release, "command") as commands:
            release.publish_files(REPOSITORY, MERGE, "0.4.1", {}, Path("notes"))
            self.assertEqual([call.args[0] for call in reads.call_args_list][-2:],
                             [f"repos/{REPOSITORY}/releases/42"] * 2)
            self.assertEqual([call.args[1:3] for call in commands.call_args_list][1:],
                             [("release", "create"), ("release", "edit")])

    def test_pagination_does_not_silently_truncate(self):
        with patch.object(release, "api", side_effect=[[1] * 100, [2]]) as request:
            self.assertEqual(len(release.listing("endpoint")), 101)
            self.assertIn("page=2", request.call_args.args[0])
        with patch.object(release, "api", return_value=[1] * 100):
            with self.assertRaises(ValueError):
                release.listing("endpoint")

    def test_unchanged_version_never_accesses_release_api(self):
        env = {"GITHUB_REPOSITORY": REPOSITORY, "GITHUB_REF": "refs/heads/main", "RELEASE_COMMIT": MERGE}
        same = '[workspace.package]\nversion="0.4.1"'
        with patch.dict(release.os.environ, env), patch.object(release, "git", side_effect=["", f"{MERGE} {BASE}", same, same]), \
                patch.object(release, "api") as network, patch.object(release, "publish_files") as publish:
            release.publish()
            network.assert_not_called()
            publish.assert_not_called()

    def publication_fixture(self, stale_tree=False, bad_parent=False):
        pr = pr_fixture()
        pr["base"]["repo"]["id"] = 5
        artifacts = self.artifacts()
        downloads = {}
        for target, item in zip(release.TARGETS, artifacts):
            data, metadata = candidate_zip({"tree": BASE} if stale_tree else None, target=target)
            item["digest"] = metadata["digest"]
            item["size_in_bytes"] = len(data)
            downloads[f"repos/{REPOSITORY}/actions/artifacts/{item['id']}/zip"] = data

        def fake_git(*args):
            if args[0] == "merge-base":
                return ""
            if args[0] == "rev-list":
                return f"{MERGE} {BASE} {HEAD}"
            if args[0] == "rev-parse":
                return TREE
            return '[workspace.package]\nversion="' + ("0.4.0" if args[1].startswith(BASE) else "0.4.1") + '"'

        def fake_listing(endpoint, key=None):
            if endpoint.endswith("/pulls"):
                return [pr]
            if key == "workflow_runs":
                return [run_fixture()]
            if key == "jobs":
                return self.jobs()
            if key == "artifacts":
                return artifacts
            self.fail(endpoint)

        def fake_api(endpoint, optional=False):
            if "/pulls/" in endpoint:
                return pr
            return {"tree": {"sha": TREE}, "parents": [{"sha": BASE}, {"sha": BASE if bad_parent else HEAD}]}

        def fake_command(*args):
            if args[:2] == ("gh", "api"):
                return downloads[args[2]]
            self.assertEqual(args[:2], ("git", "show"))
            return b"verified source file\n"

        return fake_git, fake_listing, fake_api, fake_command

    def test_full_admission_reuses_four_archives_without_execution(self):
        env = {"GITHUB_REPOSITORY": REPOSITORY, "GITHUB_REF": "refs/heads/main", "RELEASE_COMMIT": MERGE}
        fgit, flist, fapi, fcommand = self.publication_fixture()

        def check_publication(repository, commit, version, files, notes):
            self.assertEqual((repository, commit, version), (REPOSITORY, MERGE, "0.4.1"))
            self.assertEqual(len(files), 8)
            for target in release.TARGETS:
                self.assertEqual(files[release.archive_name(target)].read_bytes(), b"opaque release archive bytes")
            proof = json.loads(files["release-provenance.json"].read_text())
            self.assertEqual(proof["candidate_run"], 20)
            self.assertEqual(len(proof["artifacts"]), 4)
            self.assertEqual(len(files["SHA256SUMS"].read_text().splitlines()), 7)

        with patch.dict(release.os.environ, env), patch.object(release, "git", side_effect=fgit), \
                patch.object(release, "listing", side_effect=flist), patch.object(release, "api", side_effect=fapi), \
                patch.object(release, "command", side_effect=fcommand), \
                patch.object(release, "publish_files", side_effect=check_publication) as publish:
            release.publish()
            publish.assert_called_once()

    def test_changed_tree_or_forged_synthetic_parent_never_publishes(self):
        env = {"GITHUB_REPOSITORY": REPOSITORY, "GITHUB_REF": "refs/heads/main", "RELEASE_COMMIT": MERGE}
        for changed_tree, bad_parent in ((True, False), (False, True)):
            fgit, flist, fapi, fcommand = self.publication_fixture(changed_tree, bad_parent)
            with patch.dict(release.os.environ, env), patch.object(release, "git", side_effect=fgit), \
                    patch.object(release, "listing", side_effect=flist), patch.object(release, "api", side_effect=fapi), \
                    patch.object(release, "command", side_effect=fcommand), patch.object(release, "publish_files") as publish:
                with self.assertRaises(ValueError):
                    release.publish()
                publish.assert_not_called()


if __name__ == "__main__":
    unittest.main()
