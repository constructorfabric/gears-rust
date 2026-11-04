#!/usr/bin/env python3
"""Keep application primitives behind toolkit-security's crypto facade.

Cargo metadata resolves workspace inheritance, renamed dependencies, target
sections and dev dependencies. This checks direct dependencies, not the
presence of cryptography transitively linked by unrelated libraries.
"""
import json
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
PRIMITIVES = {
    "aws-lc-rs", "ring", "sha1", "sha2", "md-5", "hmac", "hkdf", "aes",
    "aes-gcm", "chacha20", "chacha20poly1305", "rsa", "p256", "p384", "p521",
    "ed25519-dalek", "blake2", "blake3", "openssl", "openssl-sys",
    "sha3", "md5", "k256", "ecdsa", "pbkdf2", "argon2", "scrypt",
    "ed25519", "ed25519-compact", "x25519-dalek", "curve25519-dalek",
    "security-framework", "win-crypto-ng",
}
FACADE = "libs/toolkit-security"
BACKEND = "libs/rustls-corecrypto-provider"
TEST_ORACLES = {
    (BACKEND, "rsa"), (BACKEND, "sha2"),
    ("gears/system/authn-resolver/plugins/firebase-authn-plugin", "rsa"),
}
RAW_TLS = re.compile(r"\b(?:rustls\s*::\s*crypto\s*::\s*(?:aws_lc_rs|ring|default_fips_provider)|rustls_(?:corecrypto_provider|cng_crypto)\s*::)")
RAW_JWT = re.compile(r"\bjsonwebtoken\s*::\s*(?:crypto\s*::\s*(?:verify|aws_lc\s*::\s*DEFAULT_PROVIDER)|decode\s*(?:\(|::<))")
JWT_IMPORT = re.compile(r"\buse\s+jsonwebtoken\s*::\s*(?:decode\b|\{[^;]*\bdecode\b)", re.S)
BACKEND_IMPORT = re.compile(
    r"\buse\s+(?:jsonwebtoken\s*(?:as\b|::\s*\*|::\s*\{[^;]*\bcrypto\b|::\s*crypto\b)|"
    r"rustls\s*(?:as\b|::\s*\*|::\s*\{[^;]*\bcrypto\b|::\s*crypto\s*(?:;|as\b|::\s*\*|::\s*\{[^;]*\b(?:self|aws_lc_rs|ring|default_fips_provider)\b)))", re.S)
# This integration test deliberately installs a poisoned JWT provider and uses
# an independent signing oracle. No production file is exempted for its tests.
SOURCE_ORACLES = {FACADE + "/tests/jwt_no_backend_fallback.rs"}
# Match strings before comments, so // inside an issuer URL is not a comment.
RUST_RAW_STRING = r'r(?P<hashes>#{0,255})".*?"(?P=hashes)'
RUST_STRING = r'"(?:\\.|[^"\\])*"'
RUST_COMMENT = r'(?P<comment>//[^\n]*|/\*.*?\*/)'
RUST_TOKENS = re.compile("|".join((RUST_RAW_STRING, RUST_STRING, RUST_COMMENT)), re.S)


def enables_ring(package, dependency):
    alias = dependency.get("rename") or dependency["name"]
    forwarded = {f"{alias}/ring", f"{alias}?/ring"}
    return "ring" in dependency.get("features", []) or any(
        forwarded.intersection(features) for features in package.get("features", {}).values()
    )


def allowed_primitive(path, dependency):
    name = dependency["name"]
    if path == FACADE and name in {"aws-lc-rs", "security-framework", "win-crypto-ng"}:
        return True
    if path == BACKEND and name == "security-framework":
        return True
    return dependency["kind"] == "dev" and (path, name) in TEST_ORACLES


def package_dependency_errors(package, path):
    errors = []
    for dependency in package["dependencies"]:
        name = dependency["name"]
        if name == "rustls" and enables_ring(package, dependency):
            errors.append(f"{path}/Cargo.toml: direct rustls/ring enablement is forbidden")
        if name in PRIMITIVES and not allowed_primitive(path, dependency):
            errors.append(f"{path}/Cargo.toml: use toolkit-security::crypto instead of {name}")
    return errors


def dependency_errors(metadata, root):
    errors = []
    for package in metadata["packages"]:
        if package["id"] in metadata["workspace_members"]:
            path = Path(package["manifest_path"]).parent.relative_to(root).as_posix()
            errors.extend(package_dependency_errors(package, path))
    return errors


def source_errors(path, source):
    # Backend implementation and deliberate wrong-provider test are boundaries.
    if path.startswith(FACADE + "/src/crypto/") or path.startswith(BACKEND + "/"):
        return []
    if path == FACADE + "/src/provider.rs":
        return []
    if path == "libs/toolkit/tests/crypto_init_fips.rs" or path in SOURCE_ORACLES:
        return []
    # Ignore comments, so architecture explanations are not policy violations.
    code = RUST_TOKENS.sub(lambda match: " " if match.group("comment") else '"literal"', source)
    jwt_bypass = path != FACADE + "/src/jwt.rs" and (RAW_JWT.search(code) or JWT_IMPORT.search(code))
    if RAW_TLS.search(code) or jwt_bypass or BACKEND_IMPORT.search(code):
        return [f"{path}: route primitives through toolkit-security::crypto and protocol integration through toolkit-security JWT/TLS adapters"]
    return []


def main():
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], cwd=ROOT))
    errors = dependency_errors(metadata, ROOT)
    paths = subprocess.check_output(
        ["git", "ls-files", "-co", "--exclude-standard", "--", "*.rs"], cwd=ROOT,
        text=True).splitlines()
    for path in sorted(set(paths)):
        source_path = ROOT / path
        if source_path.is_file():
            errors.extend(source_errors(path, source_path.read_text()))
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print("Crypto facade dependency and source checks passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
