/// Ephemeral minisign key pair for tests.
///
/// Keys are generated at runtime and never persisted, so no secret material
/// enters the repository.
pub struct TestKeyPair {
    keypair: minisign::KeyPair,
    public_key: String,
}

impl TestKeyPair {
    pub fn generate() -> Self {
        let keypair = minisign::KeyPair::generate_unencrypted_keypair()
            .expect("generate test minisign key pair");
        let public_key = keypair.pk.to_base64();
        Self {
            keypair,
            public_key,
        }
    }

    pub fn public_key(&self) -> &str {
        &self.public_key
    }

    /// Produce a full detached signature text over `data`.
    pub fn sign(&self, data: &[u8]) -> String {
        minisign::sign(None, &self.keypair.sk, data, None, None)
            .expect("sign test manifest")
            .into()
    }
}
