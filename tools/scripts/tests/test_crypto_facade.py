"""Policy regressions: dependency aliases/targets and new call-site paths."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("crypto_guard", Path(__file__).parents[1] / "check_crypto_facade.py")
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


class CryptoFacadePolicyTests(unittest.TestCase):
    def metadata(self, path, dependency):
        return {"workspace_members": ["app"], "packages": [{"id": "app",
            "manifest_path": str(Path("/repo") / path / "Cargo.toml"),
            "dependencies": [dependency]}]}

    def test_alias_and_target_do_not_bypass_dependency_policy(self):
        for kind in [None, "build", "dev"]:
            metadata = self.metadata("gears/new-app", {"name": "sha2", "rename": "checksum",
                "kind": kind, "target": 'cfg(target_os = "windows")'})
            self.assertTrue(guard.dependency_errors(metadata, Path("/repo")))

    def test_oracle_exception_is_dev_only_and_path_specific(self):
        dependency = {"name": "rsa", "kind": "dev"}
        self.assertFalse(guard.dependency_errors(self.metadata(guard.BACKEND, dependency), Path("/repo")))
        self.assertTrue(guard.dependency_errors(self.metadata("libs/new-provider", dependency), Path("/repo")))
        dependency["kind"] = None
        self.assertTrue(guard.dependency_errors(self.metadata(guard.BACKEND, dependency), Path("/repo")))

    def test_new_file_cannot_select_a_tls_backend_or_verify_jwt_directly(self):
        for source in ["rustls::crypto::aws_lc_rs::default_provider();",
                       "rustls_cng_crypto::fips_provider();",
                       "jsonwebtoken::decode::<Claims>(token, key, validation);",
                       "use jsonwebtoken::{Header, decode as parse};"]:
            self.assertTrue(guard.source_errors("gears/new/src/lib.rs", source))
        self.assertFalse(guard.source_errors("gears/new/src/lib.rs", "// rustls::crypto::aws_lc_rs\nuse toolkit_security::crypto::hash::Sha256;"))

    def test_aliases_groups_and_url_strings_do_not_hide_calls(self):
        for source in ["use jsonwebtoken as jwt; jwt::decode::<Claims>(token, key, validation);",
                       "use jsonwebtoken::{crypto}; crypto::verify(sig, msg, key, alg);",
                       "use rustls::crypto as backend; backend::aws_lc_rs::default_provider();",
                       "use rustls::crypto::{aws_lc_rs};",
                       'let url = "https://issuer"; jsonwebtoken::decode::<Claims>(token, key, validation);']:
            self.assertTrue(guard.source_errors("gears/new/src/lib.rs", source))

    def test_strings_and_comments_cannot_hide_following_calls(self):
        literals = [
            r'"escaped \" // still in string"',
            r'"line\n // still in string"',
            'r###"https://issuer/"quote"###',
        ]
        for literal in literals:
            prefix = f"let text = {literal}; "
            self.assertTrue(guard.source_errors("gears/new/src/lib.rs",
                prefix + "jsonwebtoken::decode::<Claims>(token, key, validation);"))
            self.assertFalse(guard.source_errors("gears/new/src/lib.rs",
                prefix + "// jsonwebtoken::decode::<Claims>(token, key, validation);"))

    def test_reviewed_module_and_provider_bypasses_are_rejected(self):
        for source in [
            "use rustls::crypto; crypto::aws_lc_rs::default_provider();",
            "use rustls::crypto::{self, CryptoProvider}; crypto::aws_lc_rs::default_provider();",
            "rustls::crypto::ring::default_provider();",
            "use jsonwebtoken::crypto; crypto::verify(sig,msg,key,alg);",
            "jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER;",
        ]:
            self.assertTrue(guard.source_errors("gears/new/src/lib.rs", source), source)
        self.assertFalse(guard.source_errors("libs/toolkit-http/src/lib.rs", "use rustls::crypto::{CryptoProvider, verify_tls13_signature};"))

    def test_direct_ring_feature_and_renamed_feature_forwarding(self):
        dependency = {"name": "rustls", "kind": None, "features": ["ring"]}
        self.assertTrue(guard.dependency_errors(self.metadata("gears/new", dependency), Path("/repo")))
        dependency.update(rename="tls", features=[])
        metadata = self.metadata("gears/new", dependency)
        metadata["packages"][0]["features"] = {"custom": ["tls?/ring"]}
        self.assertTrue(guard.dependency_errors(metadata, Path("/repo")))

    def test_source_oracle_is_explicit(self):
        source = "jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER;"
        for path in guard.SOURCE_ORACLES:
            self.assertFalse(guard.source_errors(path, source))
        self.assertTrue(guard.source_errors("libs/new/tests/test.rs", source))

    def test_protocol_adapter_does_not_allow_backend_factories(self):
        adapter = guard.FACADE + "/src/jwt.rs"
        self.assertFalse(guard.source_errors(adapter, "jsonwebtoken::decode::<Claims>(token, key, validation);"))
        self.assertTrue(guard.source_errors(adapter, "rustls::crypto::aws_lc_rs::default_provider();"))
        self.assertTrue(guard.source_errors("libs/new/src/jwt.rs", "jsonwebtoken::decode::<Claims>(token, key, validation);"))


if __name__ == "__main__":
    unittest.main()
