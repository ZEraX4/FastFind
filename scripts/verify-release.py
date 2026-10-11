"""Check a FastFind release (draft or published) the way the in-app updater will:
every update package listed in latest.json must carry a valid minisign (Ed25519)
signature from the public key in src-tauri/tauri.conf.json, for the release's version.

Usage: python scripts/verify-release.py v1.2.0 [owner/repo]

Needs the GitHub CLI (`gh`, signed in, or GH_TOKEN set) and `pip install cryptography`.
Reading a draft needs push access to the repository.
"""
import base64
import hashlib
import json
import os
import shutil
import subprocess
import sys
import urllib.parse
from pathlib import Path

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

# Platforms the updater must be able to serve; tauri-action adds installer-specific variants.
REQUIRED_PLATFORMS = ["darwin-aarch64", "darwin-x86_64", "linux-x86_64", "windows-x86_64"]

if len(sys.argv) < 2:
    sys.exit(__doc__)
tag = sys.argv[1]
repo = sys.argv[2] if len(sys.argv) > 2 else os.environ.get("GITHUB_REPOSITORY", "ZEraX4/FastFind")
gh = shutil.which("gh") or r"C:\Program Files\GitHub CLI\gh.exe"
lines = []


def say(text):
    print(text)
    lines.append(text)


def gh_api(path, binary=False):
    args = [gh, "api", path]
    if binary:
        args[2:2] = ["-H", "Accept: application/octet-stream"]
    return subprocess.run(args, capture_output=True, check=True).stdout


def finish(ok):
    say("All update signatures are valid." if ok else "**Release check failed.** Don't publish this release.")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as f:
            f.write("## Update signatures\n\n" + "\n".join(f"- {l}" for l in lines) + "\n")
    sys.exit(0 if ok else 1)


conf = json.loads((Path(__file__).resolve().parent.parent / "src-tauri" / "tauri.conf.json").read_text(encoding="utf-8"))
pub = base64.b64decode(base64.b64decode(conf["plugins"]["updater"]["pubkey"]).decode().splitlines()[1])
assert pub[:2] == b"Ed", "unexpected public key algorithm"
key_id, key = pub[2:10], Ed25519PublicKey.from_public_bytes(pub[10:42])
version = conf["version"]
say(f"Public key `{key_id[::-1].hex().upper()}`, app version {version}, release {tag}")

# /releases/tags/{tag} doesn't return drafts, so look the tag up in the list.
release = next((r for r in json.loads(gh_api(f"repos/{repo}/releases?per_page=30")) if r["tag_name"] == tag), None)
if release is None:
    say(f"No release with tag {tag} in {repo}.")
    finish(False)
assets_by_id = {str(a["id"]): a for a in release["assets"]}
assets_by_name = {a["name"]: a for a in release["assets"]}
if "latest.json" not in assets_by_name:
    say("The release has no latest.json, so installed copies would never see it.")
    finish(False)
manifest = json.loads(gh_api(f"repos/{repo}/releases/assets/{assets_by_name['latest.json']['id']}", binary=True))

ok = True
if tag != f"v{version}" or manifest["version"].removeprefix("v") != version:
    say(f"Version mismatch: tag {tag}, tauri.conf.json {version}, latest.json {manifest['version']}.")
    ok = False
missing = [p for p in REQUIRED_PLATFORMS if p not in manifest["platforms"]]
if missing:
    say(f"latest.json has no entry for {', '.join(missing)}.")
    ok = False

downloads = {}
for platform, entry in sorted(manifest["platforms"].items()):
    last = entry["url"].rsplit("/", 1)[-1]
    asset = assets_by_id.get(last) or assets_by_name.get(urllib.parse.unquote(last))
    if asset is None:
        say(f"`{platform}`: {entry['url']} is not an asset of this release.")
        ok = False
        continue
    sig_lines = base64.b64decode(entry["signature"]).decode().splitlines()
    sig = base64.b64decode(sig_lines[1])
    trusted = sig_lines[2].removeprefix("trusted comment: ")
    global_sig = base64.b64decode(sig_lines[3])
    alg, sig_key_id, sig_bytes = sig[:2], sig[2:10], sig[10:74]
    # Several platforms share one package (the macOS build is universal).
    if asset["id"] not in downloads:
        downloads[asset["id"]] = gh_api(f"repos/{repo}/releases/assets/{asset['id']}", binary=True)
    data = downloads[asset["id"]]
    problems = []
    if sig_key_id != key_id:
        problems.append(f"signed with key {sig_key_id[::-1].hex().upper()}, not the configured key")
    else:
        try:
            # "ED" signs the BLAKE2b-512 hash of the file; legacy "Ed" signs the file itself.
            key.verify(sig_bytes, hashlib.blake2b(data, digest_size=64).digest() if alg == b"ED" else data)
            key.verify(global_sig, sig_bytes + trusted.encode())
        except InvalidSignature:
            problems.append("signature does not match the file")
    if f"version:{version}" not in trusted.split("\t"):
        problems.append(f"signed for a different version ({trusted})")
    if problems:
        ok = False
        say(f"`{platform}`: {asset['name']}: FAILED, {'; '.join(problems)}")
    else:
        say(f"`{platform}`: {asset['name']} ({len(data) / 1e6:.1f} MB) ok")

finish(ok)
