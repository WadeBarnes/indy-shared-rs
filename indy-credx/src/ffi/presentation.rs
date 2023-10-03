use std::collections::HashMap;
use std::convert::TryInto;

use ffi_support::FfiStr;

use super::error::{catch_error, ErrorCode};
use super::object::{IndyObject, IndyObjectId, IndyObjectList, ObjectHandle};
use super::util::{FfiList, FfiStrList};
use crate::error::Result;
use crate::services::{
    prover::create_presentation,
    types::{PresentCredentials, Presentation, RevocationRegistryDefinition},
    verifier::_verify_presentation,
};

impl_indy_object!(Presentation, "Presentation");
impl_indy_object_from_json!(Presentation, credx_presentation_from_json);

#[derive(Debug)]
#[repr(C)]
pub struct FfiCredentialEntry {
    credential: ObjectHandle,
    timestamp: i64,
    rev_state: ObjectHandle,
}

impl FfiCredentialEntry {
    fn load(&self) -> Result<CredentialEntry> {
        let credential = self.credential.load()?;
        let timestamp = if self.timestamp < 0 {
            None
        } else {
            Some(self.timestamp as u64)
        };
        let rev_state = self.rev_state.opt_load()?;
        Ok(CredentialEntry {
            credential,
            timestamp,
            rev_state,
        })
    }
}

#[derive(Debug)]
#[repr(C)]
pub struct FfiCredentialProve<'a> {
    entry_idx: i64,
    referent: FfiStr<'a>,
    is_predicate: i8,
    reveal: i8,
}

struct CredentialEntry {
    credential: IndyObject,
    timestamp: Option<u64>,
    rev_state: Option<IndyObject>,
}

#[no_mangle]
pub extern "C" fn credx_create_presentation(
    pres_req: ObjectHandle,
    credentials: FfiList<FfiCredentialEntry>,
    credentials_prove: FfiList<FfiCredentialProve>,
    self_attest_names: FfiStrList,
    self_attest_values: FfiStrList,
    link_secret: ObjectHandle,
    schemas: FfiList<ObjectHandle>,
    cred_defs: FfiList<ObjectHandle>,
    presentation_p: *mut ObjectHandle,
) -> ErrorCode {
    catch_error(|| {
        check_useful_c_ptr!(presentation_p);
        if self_attest_names.len() != self_attest_values.len() {
            return Err(err_msg!(
                "Inconsistent lengths for self-attested value parameters"
            ));
        }

        let entries = credentials.try_collect(|entry| entry.load())?;
        let schemas = IndyObjectList::load(schemas.as_slice()?)?;
        let cred_defs = IndyObjectList::load(cred_defs.as_slice()?)?;

        let self_attested = if !self_attest_names.is_empty() {
            let mut self_attested = HashMap::new();
            for (name, raw) in self_attest_names
                .as_slice()?
                .iter()
                .zip(self_attest_values.as_slice()?)
            {
                let name = name
                    .as_opt_str()
                    .ok_or_else(|| err_msg!("Missing attribute name"))?
                    .to_string();
                let raw = raw
                    .as_opt_str()
                    .ok_or_else(|| err_msg!("Missing attribute raw value"))?
                    .to_string();
                self_attested.insert(name, raw);
            }
            Some(self_attested)
        } else {
            None
        };

        let mut present_creds = PresentCredentials::default();

        for (entry_idx, entry) in entries.iter().enumerate() {
            let mut add_cred = present_creds.add_credential(
                entry.credential.cast_ref()?,
                entry.timestamp,
                entry
                    .rev_state
                    .as_ref()
                    .map(IndyObject::cast_ref)
                    .transpose()?,
            );

            for prove in credentials_prove.as_slice()? {
                if prove.entry_idx < 0 {
                    return Err(err_msg!("Invalid credential index"));
                }
                if prove.entry_idx as usize != entry_idx {
                    continue;
                }

                let referent = prove
                    .referent
                    .as_opt_str()
                    .ok_or_else(|| err_msg!("Missing referent for credential proof info"))?
                    .to_string();

                if prove.is_predicate == 0 {
                    add_cred.add_requested_attribute(referent, prove.reveal != 0);
                } else {
                    add_cred.add_requested_predicate(referent);
                }
            }
        }

        let presentation = create_presentation(
            pres_req.load()?.cast_ref()?,
            present_creds,
            self_attested,
            link_secret.load()?.cast_ref()?,
            &schemas.refs_map()?,
            &cred_defs.refs_map()?,
        )?;
        let presentation = ObjectHandle::create(presentation)?;
        unsafe { *presentation_p = presentation };
        Ok(())
    })
}

#[derive(Debug)]
#[repr(C)]
pub struct FfiRevocationEntry {
    def_entry_idx: i64,
    entry: ObjectHandle,
    timestamp: i64,
}

impl FfiRevocationEntry {
    fn load(&self) -> Result<(usize, IndyObject, u64)> {
        let def_entry_idx = self
            .def_entry_idx
            .try_into()
            .map_err(|_| err_msg!("Invalid revocation registry entry index"))?;
        let entry = self.entry.load()?;
        let timestamp = self
            .timestamp
            .try_into()
            .map_err(|_| err_msg!("Invalid timestamp for revocation entry"))?;
        Ok((def_entry_idx, entry, timestamp))
    }
}

#[no_mangle]
pub extern "C" fn credx_verify_presentation(
    presentation: ObjectHandle,
    pres_req: ObjectHandle,
    schemas: FfiList<ObjectHandle>,
    cred_defs: FfiList<ObjectHandle>,
    rev_reg_defs: FfiList<ObjectHandle>,
    rev_reg_entries: FfiList<FfiRevocationEntry>,
    result_p: *mut i8,
) -> ErrorCode {
    _credx_verify_presentation(
        presentation,
        pres_req,
        schemas,
        cred_defs,
        rev_reg_defs,
        rev_reg_entries,
        false,
        result_p,
    )
}

#[no_mangle]
pub extern "C" fn credx_verify_presentation_legacy(
    presentation: ObjectHandle,
    pres_req: ObjectHandle,
    schemas: FfiList<ObjectHandle>,
    cred_defs: FfiList<ObjectHandle>,
    rev_reg_defs: FfiList<ObjectHandle>,
    rev_reg_entries: FfiList<FfiRevocationEntry>,
    result_p: *mut i8,
) -> ErrorCode {
    _credx_verify_presentation(
        presentation,
        pres_req,
        schemas,
        cred_defs,
        rev_reg_defs,
        rev_reg_entries,
        true,
        result_p,
    )
}

#[allow(clippy::too_many_arguments)]
fn _credx_verify_presentation(
    presentation: ObjectHandle,
    pres_req: ObjectHandle,
    schemas: FfiList<ObjectHandle>,
    cred_defs: FfiList<ObjectHandle>,
    rev_reg_defs: FfiList<ObjectHandle>,
    rev_reg_entries: FfiList<FfiRevocationEntry>,
    accept_legacy_revocation: bool,
    result_p: *mut i8,
) -> ErrorCode {
    catch_error(|| {
        let schemas = IndyObjectList::load(schemas.as_slice()?)?;
        let cred_defs = IndyObjectList::load(cred_defs.as_slice()?)?;
        let rev_reg_defs = IndyObjectList::load(rev_reg_defs.as_slice()?)?;
        let rev_reg_entries = rev_reg_entries.try_collect(|entry| entry.load())?;
        let mut rev_regs = HashMap::new();
        for (idx, entry, timestamp) in rev_reg_entries.iter() {
            if *idx > rev_reg_defs.len() {
                return Err(err_msg!("Invalid revocation registry entry index"));
            }
            let id = rev_reg_defs[*idx]
                .cast_ref::<RevocationRegistryDefinition>()?
                .get_id();
            rev_regs
                .entry(id)
                .or_insert_with(HashMap::new)
                .insert(*timestamp, entry.cast_ref()?);
        }
        let verify = _verify_presentation(
            presentation.load()?.cast_ref()?,
            pres_req.load()?.cast_ref()?,
            &schemas.refs_map()?,
            &cred_defs.refs_map()?,
            Some(&rev_reg_defs.refs_map()?),
            Some(&rev_regs),
            accept_legacy_revocation,
        )?;
        unsafe { *result_p = verify as i8 };
        Ok(())
    })
}
