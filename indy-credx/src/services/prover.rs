use std::collections::{HashMap, HashSet};

use super::types::*;
use crate::anoncreds_clsignatures::{
    CredentialPublicKey, Issuer as ClIssuer, Prover as ClProver,
    RevocationRegistry as CryptoRevocationRegistry, SubProofRequest, Verifier as ClVerifier,
    Witness,
};
use crate::error::Result;
use crate::services::helpers::*;
use indy_data_types::anoncreds::{
    credential::AttributeValues,
    pres_request::{PresentationRequestPayload, RequestedAttributeInfo, RequestedPredicateInfo},
    presentation::{
        AttributeValue, Identifier, RequestedProof, RevealedAttributeGroupInfo,
        RevealedAttributeInfo, SubProofReferent,
    },
};
use indy_data_types::{Qualifiable, Validatable};

use super::tails::TailsFileReader;

pub fn create_link_secret() -> Result<LinkSecret> {
    LinkSecret::new().map_err(err_map!(Unexpected))
}

pub fn create_credential_request(
    prover_did: &DidValue,
    cred_def: &CredentialDefinition,
    link_secret: &LinkSecret,
    link_secret_id: &str,
    credential_offer: &CredentialOffer,
) -> Result<(CredentialRequest, CredentialRequestMetadata)> {
    trace!(
        "create_credential_request >>> cred_def: {:?}, link_secret: {:?}, credential_offer: {:?}",
        cred_def,
        secret!(&link_secret),
        credential_offer
    );

    let cred_def = match cred_def {
        CredentialDefinition::CredentialDefinitionV1(cd) => cd,
    };
    let credential_pub_key = CredentialPublicKey::build_from_parts(
        &cred_def.value.primary,
        cred_def.value.revocation.as_ref(),
    )?;
    let mut credential_values_builder = ClIssuer::new_credential_values_builder()?;
    credential_values_builder.add_value_hidden("master_secret", &link_secret.value.value()?)?;
    let cred_values = credential_values_builder.finalize()?;

    let nonce = new_nonce()?;
    let nonce_copy = nonce.try_clone().map_err(err_map!(Unexpected))?;

    let (blinded_ms, master_secret_blinding_data, blinded_ms_correctness_proof) =
        ClProver::blind_credential_secrets(
            &credential_pub_key,
            &credential_offer.key_correctness_proof,
            &cred_values,
            credential_offer.nonce.as_native(),
        )?;

    let credential_request = CredentialRequest {
        prover_did: prover_did.clone(),
        cred_def_id: credential_offer.cred_def_id.clone(),
        blinded_ms,
        blinded_ms_correctness_proof,
        nonce,
    };

    let credential_request_metadata = CredentialRequestMetadata {
        master_secret_blinding_data,
        nonce: nonce_copy,
        master_secret_name: link_secret_id.to_string(),
    };

    trace!(
        "create_credential_request <<< credential_request: {:?}, credential_request_metadata: {:?}",
        credential_request,
        credential_request_metadata
    );

    Ok((credential_request, credential_request_metadata))
}

pub fn process_credential(
    credential: &mut Credential,
    cred_request_metadata: &CredentialRequestMetadata,
    link_secret: &LinkSecret,
    cred_def: &CredentialDefinition,
    rev_reg_def: Option<&RevocationRegistryDefinition>,
) -> Result<()> {
    trace!("process_credential >>> credential: {:?}, cred_request_metadata: {:?}, link_secret: {:?}, cred_def: {:?}, rev_reg_def: {:?}",
            credential, cred_request_metadata, secret!(&link_secret), cred_def, rev_reg_def);

    let cred_def = match cred_def {
        CredentialDefinition::CredentialDefinitionV1(cd) => cd,
    };
    let credential_pub_key = CredentialPublicKey::build_from_parts(
        &cred_def.value.primary,
        cred_def.value.revocation.as_ref(),
    )?;
    let credential_values =
        build_credential_values(&credential.values.0, Some(&link_secret.value))?;
    let rev_pub_key = match rev_reg_def {
        Some(RevocationRegistryDefinition::RevocationRegistryDefinitionV1(def)) => {
            Some(&def.value.public_keys.accum_key)
        }
        _ => None,
    };

    ClProver::process_credential_signature(
        &mut credential.signature,
        &credential_values,
        &credential.signature_correctness_proof,
        &cred_request_metadata.master_secret_blinding_data,
        &credential_pub_key,
        cred_request_metadata.nonce.as_native(),
        rev_pub_key,
        credential.rev_reg.as_ref(),
        credential.witness.as_ref(),
    )?;

    trace!("process_credential <<< ");

    Ok(())
}

pub fn create_presentation(
    pres_req: &PresentationRequest,
    credentials: PresentCredentials,
    self_attested: Option<HashMap<String, String>>,
    link_secret: &LinkSecret,
    schemas: &HashMap<SchemaId, &Schema>,
    cred_defs: &HashMap<CredentialDefinitionId, &CredentialDefinition>,
) -> Result<Presentation> {
    trace!("create_proof >>> credentials: {:?}, pres_req: {:?}, credentials: {:?}, self_attested: {:?}, link_secret: {:?}, schemas: {:?}, cred_defs: {:?}",
            credentials, pres_req, credentials, &self_attested, secret!(&link_secret), schemas, cred_defs);

    if credentials.is_empty()
        && self_attested
            .as_ref()
            .map(HashMap::is_empty)
            .unwrap_or(true)
    {
        return Err(err_msg!(
            "No credential mapping or self-attested attributes presented"
        ));
    }
    // check for duplicate referents
    credentials.validate()?;

    let pres_req_val = pres_req.value();
    let mut proof_builder = ClProver::new_proof_builder()?;
    proof_builder.add_common_attribute("master_secret")?;

    let mut requested_proof = RequestedProof::default();

    requested_proof.self_attested_attrs = self_attested.unwrap_or_default();

    let mut sub_proof_index = 0;
    let non_credential_schema = build_non_credential_schema()?;

    let mut identifiers: Vec<Identifier> = Vec::with_capacity(credentials.len());
    for present in credentials.0 {
        if present.is_empty() {
            continue;
        }
        let credential = present.cred;

        let schema = *schemas
            .get(&credential.schema_id)
            .ok_or_else(|| err_msg!("Schema not provided for ID: {}", credential.schema_id))?;
        let schema = match schema {
            Schema::SchemaV1(schema) => schema,
        };

        let cred_def = *cred_defs.get(&credential.cred_def_id).ok_or_else(|| {
            err_msg!(
                "Credential Definition not provided for ID: {}",
                credential.cred_def_id
            )
        })?;
        let cred_def = match cred_def {
            CredentialDefinition::CredentialDefinitionV1(cd) => cd,
        };

        let credential_pub_key = CredentialPublicKey::build_from_parts(
            &cred_def.value.primary,
            cred_def.value.revocation.as_ref(),
        )?;

        let credential_schema = build_credential_schema(&schema.attr_names.0)?;
        let credential_values =
            build_credential_values(&credential.values.0, Some(&link_secret.value))?;
        let (req_attrs, req_predicates) = prepare_credential_for_proving(
            present.requested_attributes,
            present.requested_predicates,
            pres_req_val,
        )?;
        let sub_proof_request = build_sub_proof_request(&req_attrs, &req_predicates)?;

        proof_builder.add_sub_proof_request(
            &sub_proof_request,
            &credential_schema,
            &non_credential_schema,
            &credential.signature,
            &credential_values,
            &credential_pub_key,
            present.rev_state.as_ref().map(|r_info| &r_info.rev_reg),
            present.rev_state.as_ref().map(|r_info| &r_info.witness),
        )?;

        let identifier = match pres_req {
            PresentationRequest::PresentationRequestV1(_) => Identifier {
                schema_id: credential.schema_id.to_unqualified(),
                cred_def_id: credential.cred_def_id.to_unqualified(),
                rev_reg_id: credential.rev_reg_id.as_ref().map(|id| id.to_unqualified()),
                timestamp: present.timestamp,
            },
            PresentationRequest::PresentationRequestV2(_) => Identifier {
                schema_id: credential.schema_id.clone(),
                cred_def_id: credential.cred_def_id.clone(),
                rev_reg_id: credential.rev_reg_id.clone(),
                timestamp: present.timestamp,
            },
        };

        identifiers.push(identifier);

        update_requested_proof(
            req_attrs,
            req_predicates,
            pres_req_val,
            credential,
            sub_proof_index,
            &mut requested_proof,
        )?;

        sub_proof_index += 1;
    }

    let proof = proof_builder.finalize(pres_req_val.nonce.as_native())?;

    let full_proof = Presentation {
        proof,
        requested_proof,
        identifiers,
    };

    trace!("create_proof <<< full_proof: {:?}", secret!(&full_proof));

    Ok(full_proof)
}

pub fn create_or_update_revocation_state(
    tails_reader: TailsFileReader,
    revoc_reg_def: &RevocationRegistryDefinition,
    rev_reg_delta: &RevocationRegistryDelta,
    rev_reg_idx: u32,
    timestamp: u64,
    rev_state: Option<&CredentialRevocationState>,
) -> Result<CredentialRevocationState> {
    trace!(
        "create_or_update_revocation_state >>> , tails_reader: {:?}, revoc_reg_def: {:?}, \
rev_reg_delta: {:?}, rev_reg_idx: {}, timestamp: {:?}, rev_state: {:?}",
        tails_reader,
        revoc_reg_def,
        rev_reg_delta,
        rev_reg_idx,
        timestamp,
        rev_state
    );

    let revoc_reg_def = match revoc_reg_def {
        RevocationRegistryDefinition::RevocationRegistryDefinitionV1(v1) => v1,
    };
    let rev_reg_delta = match rev_reg_delta {
        RevocationRegistryDelta::RevocationRegistryDeltaV1(v1) => v1,
    };

    let witness = match rev_state {
        None => Witness::new(
            rev_reg_idx,
            revoc_reg_def.value.max_cred_num,
            revoc_reg_def.value.issuance_type.to_bool(),
            &rev_reg_delta.value,
            &tails_reader,
        )?,
        Some(source_rev_state) => {
            let mut witness = source_rev_state.witness.clone();
            witness.update(
                rev_reg_idx,
                revoc_reg_def.value.max_cred_num,
                &rev_reg_delta.value,
                &tails_reader,
            )?;
            witness
        }
    };

    Ok(CredentialRevocationState {
        witness,
        rev_reg: CryptoRevocationRegistry::from(rev_reg_delta.value.clone()),
        timestamp,
    })
}

fn prepare_credential_for_proving(
    requested_attributes: HashSet<(String, bool)>,
    requested_predicates: HashSet<String>,
    pres_req: &PresentationRequestPayload,
) -> Result<(Vec<RequestedAttributeInfo>, Vec<RequestedPredicateInfo>)> {
    trace!(
        "_prepare_credentials_for_proving >>> requested_attributes: {:?}, requested_predicates: {:?}, pres_req: {:?}",
        requested_attributes,
        requested_predicates,
        pres_req
    );

    let mut attrs = Vec::with_capacity(requested_attributes.len());
    let mut preds = Vec::with_capacity(requested_predicates.len());

    for (attr_referent, revealed) in requested_attributes {
        let attr_info = pres_req
            .requested_attributes
            .get(attr_referent.as_str())
            .ok_or_else(|| {
                err_msg!(
                    "AttributeInfo not found in PresentationRequest for referent \"{}\"",
                    attr_referent.as_str()
                )
            })?;

        attrs.push(RequestedAttributeInfo {
            attr_referent,
            attr_info: attr_info.clone(),
            revealed,
        });
    }

    for predicate_referent in requested_predicates {
        let predicate_info = pres_req
            .requested_predicates
            .get(predicate_referent.as_str())
            .ok_or_else(|| {
                err_msg!(
                    "PredicateInfo not found in PresentationRequest for referent \"{}\"",
                    predicate_referent.as_str()
                )
            })?;

        preds.push(RequestedPredicateInfo {
            predicate_referent,
            predicate_info: predicate_info.clone(),
        });
    }

    trace!(
        "_prepare_credential_for_proving <<< attrs: {:?}, preds: {:?}",
        attrs,
        preds
    );

    Ok((attrs, preds))
}

fn get_credential_values_for_attribute(
    credential_attrs: &HashMap<String, AttributeValues>,
    requested_attr: &str,
) -> Option<AttributeValues> {
    trace!(
        "get_credential_values_for_attribute >>> credential_attrs: {:?}, requested_attr: {:?}",
        secret!(credential_attrs),
        requested_attr
    );

    let res = credential_attrs
        .iter()
        .find(|&(ref key, _)| attr_common_view(key) == attr_common_view(&requested_attr))
        .map(|(_, values)| values.clone());

    trace!(
        "get_credential_values_for_attribute <<< res: {:?}",
        secret!(&res)
    );

    res
}

fn update_requested_proof(
    req_attrs_for_credential: Vec<RequestedAttributeInfo>,
    req_predicates_for_credential: Vec<RequestedPredicateInfo>,
    proof_req: &PresentationRequestPayload,
    credential: &Credential,
    sub_proof_index: u32,
    requested_proof: &mut RequestedProof,
) -> Result<()> {
    trace!("_update_requested_proof >>> req_attrs_for_credential: {:?}, req_predicates_for_credential: {:?}, proof_req: {:?}, credential: {:?}, \
           sub_proof_index: {:?}, requested_proof: {:?}",
           req_attrs_for_credential, req_predicates_for_credential, proof_req, secret!(&credential), sub_proof_index, secret!(&requested_proof));

    for attr_info in req_attrs_for_credential {
        if attr_info.revealed {
            let attribute = &proof_req.requested_attributes[&attr_info.attr_referent];

            if let Some(name) = &attribute.name {
                let attribute_values =
                    get_credential_values_for_attribute(&credential.values.0, &name).ok_or_else(
                        || err_msg!("Credential value not found for attribute {:?}", name),
                    )?;

                requested_proof.revealed_attrs.insert(
                    attr_info.attr_referent.clone(),
                    RevealedAttributeInfo {
                        sub_proof_index,
                        raw: attribute_values.raw,
                        encoded: attribute_values.encoded,
                    },
                );
            } else if let Some(names) = &attribute.names {
                let mut value_map: HashMap<String, AttributeValue> = HashMap::new();
                for name in names {
                    let attr_value =
                        get_credential_values_for_attribute(&credential.values.0, &name)
                            .ok_or_else(|| {
                                err_msg!("Credential value not found for attribute {:?}", name)
                            })?;
                    value_map.insert(
                        name.clone(),
                        AttributeValue {
                            raw: attr_value.raw,
                            encoded: attr_value.encoded,
                        },
                    );
                }
                requested_proof.revealed_attr_groups.insert(
                    attr_info.attr_referent.clone(),
                    RevealedAttributeGroupInfo {
                        sub_proof_index,
                        values: value_map,
                    },
                );
            }
        } else {
            requested_proof.unrevealed_attrs.insert(
                attr_info.attr_referent,
                SubProofReferent { sub_proof_index },
            );
        }
    }

    for predicate_info in req_predicates_for_credential {
        requested_proof.predicates.insert(
            predicate_info.predicate_referent,
            SubProofReferent { sub_proof_index },
        );
    }

    trace!("_update_requested_proof <<<");

    Ok(())
}

fn build_sub_proof_request(
    req_attrs_for_credential: &[RequestedAttributeInfo],
    req_predicates_for_credential: &[RequestedPredicateInfo],
) -> Result<SubProofRequest> {
    trace!("_build_sub_proof_request <<< req_attrs_for_credential: {:?}, req_predicates_for_credential: {:?}",
           req_attrs_for_credential, req_predicates_for_credential);

    let mut sub_proof_request_builder = ClVerifier::new_sub_proof_request_builder()?;

    for attr in req_attrs_for_credential {
        if attr.revealed {
            if let Some(ref name) = &attr.attr_info.name {
                sub_proof_request_builder.add_revealed_attr(&attr_common_view(name))?
            } else if let Some(ref names) = &attr.attr_info.names {
                for name in names {
                    sub_proof_request_builder.add_revealed_attr(&attr_common_view(name))?
                }
            }
        }
    }

    for predicate in req_predicates_for_credential {
        let p_type = format!("{}", predicate.predicate_info.p_type);

        sub_proof_request_builder.add_predicate(
            &attr_common_view(&predicate.predicate_info.name),
            &p_type,
            predicate.predicate_info.p_value,
        )?;
    }

    let sub_proof_request = sub_proof_request_builder.finalize()?;

    trace!(
        "_build_sub_proof_request <<< sub_proof_request: {:?}",
        sub_proof_request
    );

    Ok(sub_proof_request)
}

#[cfg(test)]
mod tests {
    use super::*;

    use indy_data_types::anoncreds::pres_request::PredicateTypes;

    macro_rules! hashmap {
        ($( $key: expr => $val: expr ),*) => {
            {
                let mut map = ::std::collections::HashMap::new();
                $(
                    map.insert($key, $val);
                )*
                map
            }
        }
    }

    macro_rules! hashset {
        ($( $val: expr ),*) => {
            {
                let mut set = ::std::collections::HashSet::new();
                $(
                    set.insert($val);
                )*
                set
            }
        }
    }

    mod prepare_credentials_for_proving {
        use indy_data_types::anoncreds::pres_request::{AttributeInfo, PredicateInfo};

        use super::*;

        const ATTRIBUTE_REFERENT: &str = "attribute_referent";
        const PREDICATE_REFERENT: &str = "predicate_referent";

        fn _attr_info() -> AttributeInfo {
            AttributeInfo {
                name: Some("name".to_string()),
                names: None,
                restrictions: None,
                non_revoked: None,
            }
        }

        fn _predicate_info() -> PredicateInfo {
            PredicateInfo {
                name: "age".to_string(),
                p_type: PredicateTypes::GE,
                p_value: 8,
                restrictions: None,
                non_revoked: None,
            }
        }

        fn _proof_req() -> PresentationRequestPayload {
            PresentationRequestPayload {
                nonce: new_nonce().unwrap(),
                name: "Job-Application".to_string(),
                version: "0.1".to_string(),
                requested_attributes: hashmap!(
                    ATTRIBUTE_REFERENT.to_string() => _attr_info()
                ),
                requested_predicates: hashmap!(
                    PREDICATE_REFERENT.to_string() => _predicate_info()
                ),
                non_revoked: None,
            }
        }

        fn _req_cred() -> (HashSet<(String, bool)>, HashSet<String>) {
            (
                hashset!((ATTRIBUTE_REFERENT.to_string(), false)),
                hashset!(PREDICATE_REFERENT.to_string()),
            )
        }

        #[test]
        fn prepare_credential_for_proving_works() {
            let (req_attrs, req_preds) = _req_cred();
            let proof_req = _proof_req();

            let (req_attr_info, req_pred_info) =
                prepare_credential_for_proving(req_attrs, req_preds, &proof_req).unwrap();

            assert_eq!(1, req_attr_info.len());
            assert_eq!(1, req_pred_info.len());
        }

        #[test]
        fn prepare_credential_for_proving_works_for_multiple_attributes() {
            let (mut req_attrs, req_preds) = _req_cred();
            let mut proof_req = _proof_req();

            req_attrs.insert(("attribute_referent_2".to_string(), false));

            proof_req.requested_attributes.insert(
                "attribute_referent_2".to_string(),
                AttributeInfo {
                    name: Some("last_name".to_string()),
                    names: None,
                    restrictions: None,
                    non_revoked: None,
                },
            );

            let (req_attr_info, req_pred_info) =
                prepare_credential_for_proving(req_attrs, req_preds, &proof_req).unwrap();

            assert_eq!(2, req_attr_info.len());
            assert_eq!(1, req_pred_info.len());
        }

        #[test]
        fn prepare_credential_for_proving_works_for_missed_attribute() {
            let (req_attrs, req_preds) = _req_cred();
            let mut proof_req = _proof_req();

            proof_req.requested_attributes.clear();

            let res = prepare_credential_for_proving(req_attrs, req_preds, &proof_req);
            assert_kind!(Input, res);
        }

        #[test]
        fn prepare_credential_for_proving_works_for_missed_predicate() {
            let (req_attrs, req_preds) = _req_cred();
            let mut proof_req = _proof_req();

            proof_req.requested_predicates.clear();

            let res = prepare_credential_for_proving(req_attrs, req_preds, &proof_req);
            assert_kind!(Input, res);
        }
    }

    mod get_credential_values_for_attribute {
        use super::*;

        fn _attr_values() -> AttributeValues {
            AttributeValues {
                raw: "Alex".to_string(),
                encoded: "123".to_string(),
            }
        }

        fn _cred_values() -> HashMap<String, AttributeValues> {
            hashmap!("name".to_string() => _attr_values())
        }

        #[test]
        fn get_credential_values_for_attribute_works() {
            let res = get_credential_values_for_attribute(&_cred_values(), "name").unwrap();
            assert_eq!(_attr_values(), res);
        }

        #[test]
        fn get_credential_values_for_attribute_works_for_requested_attr_different_case() {
            let res = get_credential_values_for_attribute(&_cred_values(), "NAme").unwrap();
            assert_eq!(_attr_values(), res);
        }

        #[test]
        fn get_credential_values_for_attribute_works_for_requested_attr_contains_spaces() {
            let res = get_credential_values_for_attribute(&_cred_values(), "   na me  ").unwrap();
            assert_eq!(_attr_values(), res);
        }

        #[test]
        fn get_credential_values_for_attribute_works_for_cred_values_different_case() {
            let cred_values = hashmap!("NAME".to_string() => _attr_values());

            let res = get_credential_values_for_attribute(&cred_values, "name").unwrap();
            assert_eq!(_attr_values(), res);
        }

        #[test]
        fn get_credential_values_for_attribute_works_for_cred_values_contains_spaces() {
            let cred_values = hashmap!("    name    ".to_string() => _attr_values());

            let res = get_credential_values_for_attribute(&cred_values, "name").unwrap();
            assert_eq!(_attr_values(), res);
        }

        #[test]
        fn get_credential_values_for_attribute_works_for_cred_values_and_requested_attr_contains_spaces(
        ) {
            let cred_values = hashmap!("    name    ".to_string() => _attr_values());

            let res =
                get_credential_values_for_attribute(&cred_values, "            name            ")
                    .unwrap();
            assert_eq!(_attr_values(), res);
        }
    }
}
