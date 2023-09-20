use indy_credx::types::{CredentialDefinitionPrivate, CredentialKeyCorrectnessProof};

use indy_data_types::anoncreds::cred_def::CredentialDefinition;
use indy_data_types::anoncreds::credential::Credential;
use indy_data_types::anoncreds::link_secret::LinkSecret;
use indy_data_types::did::DidValue;

pub const ISSUER_DID: &'static str = "NcYxiDXkpYi6ov5FcYDi1e";
pub const PROVER_DID: &'static str = "VsKV7grR1BUE29mG2Fm2kX";

pub struct StoredCredDef {
    pub public: CredentialDefinition,
    pub private: CredentialDefinitionPrivate,
    pub key_proof: CredentialKeyCorrectnessProof,
}

impl
    From<(
        CredentialDefinition,
        CredentialDefinitionPrivate,
        CredentialKeyCorrectnessProof,
    )> for StoredCredDef
{
    fn from(
        parts: (
            CredentialDefinition,
            CredentialDefinitionPrivate,
            CredentialKeyCorrectnessProof,
        ),
    ) -> Self {
        let (public, private, key_proof) = parts;
        Self {
            public,
            private,
            key_proof,
        }
    }
}

// A struct for keeping all issuer-related objects together
pub struct IssuerWallet {
    pub did: DidValue,
    pub cred_defs: Vec<StoredCredDef>,
}

impl Default for IssuerWallet {
    fn default() -> Self {
        Self {
            did: DidValue::from(ISSUER_DID.to_string()),
            cred_defs: vec![],
        }
    }
}

// A struct for keeping all issuer-related objects together
pub struct ProverWallet {
    pub did: DidValue,
    pub credentials: Vec<Credential>,
    pub link_secret: LinkSecret,
}

impl Default for ProverWallet {
    fn default() -> Self {
        let link_secret = LinkSecret::new().expect("Error creating prover link secret");
        Self {
            did: DidValue::from(PROVER_DID.to_string()),
            credentials: vec![],
            link_secret,
        }
    }
}
