use super::nonce::Nonce;
use crate::did::DidValue;
use crate::identifiers::cred_def::CredentialDefinitionId;
use crate::{Qualifiable, Validatable, ValidationError};

#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
pub struct CredentialRequest {
    pub prover_did: DidValue,
    pub cred_def_id: CredentialDefinitionId,
    pub blinded_ms: cl_type!(BlindedCredentialSecrets),
    pub blinded_ms_correctness_proof: cl_type!(BlindedCredentialSecretsCorrectnessProof),
    pub nonce: Nonce,
}

impl CredentialRequest {
    #[allow(unused)]
    pub fn to_unqualified(self) -> CredentialRequest {
        CredentialRequest {
            prover_did: self.prover_did.to_unqualified(),
            cred_def_id: self.cred_def_id.to_unqualified(),
            blinded_ms: self.blinded_ms,
            blinded_ms_correctness_proof: self.blinded_ms_correctness_proof,
            nonce: self.nonce,
        }
    }
}

impl Validatable for CredentialRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        self.cred_def_id.validate()?;
        self.prover_did.validate()?;
        Ok(())
    }
}

#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
pub struct CredentialRequestMetadata {
    pub master_secret_blinding_data: cl_type!(CredentialSecretsBlindingFactors),
    pub nonce: Nonce,
    pub master_secret_name: String,
}

impl Validatable for CredentialRequestMetadata {}
