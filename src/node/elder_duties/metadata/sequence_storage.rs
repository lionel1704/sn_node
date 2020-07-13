// Copyright 2020 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

use crate::{
    chunk_store::{error::Error as ChunkStoreError, SequenceChunkStore},
    cmd::OutboundMsg,
    node::keys::NodeKeys,
    node::msg_decisions::ElderMsgDecisions,
    node::Init,
    Config, Result,
};
use safe_nd::{
    CmdError, Error as NdError, Message, MessageId, MsgSender, QueryResponse, Result as NdResult,
    SData, SDataAction, SDataAddress, SDataEntry, SDataIndex, SDataOwner, SDataPermissions,
    SDataPrivPermissions, SDataPubPermissions, SDataUser, SDataWriteOp, SequenceRead,
    SequenceWrite,
};
use std::{
    cell::Cell,
    fmt::{self, Display, Formatter},
    rc::Rc,
};

pub(super) struct SequenceStorage {
    keys: NodeKeys,
    chunks: SequenceChunkStore,
    decisions: ElderMsgDecisions,
}

impl SequenceStorage {
    pub(super) fn new(
        keys: NodeKeys,
        config: &Config,
        total_used_space: &Rc<Cell<u64>>,
        init_mode: Init,
        decisions: ElderMsgDecisions,
    ) -> Result<Self> {
        let root_dir = config.root_dir()?;
        let max_capacity = config.max_capacity();
        let chunks = SequenceChunkStore::new(
            &root_dir,
            max_capacity,
            Rc::clone(total_used_space),
            init_mode,
        )?;
        Ok(Self {
            keys,
            chunks,
            decisions,
        })
    }

    pub(super) fn read(
        &self,
        read: &SequenceRead,
        msg_id: MessageId,
        origin: &MsgSender,
    ) -> Option<OutboundMsg> {
        use SequenceRead::*;
        match read {
            Get(address) => self.get(*address, msg_id, &origin),
            GetRange { address, range } => self.get_range(*address, *range, msg_id, &origin),
            GetLastEntry(address) => self.get_last_entry(*address, msg_id, &origin),
            GetOwner(address) => self.get_owner(*address, msg_id, &origin),
            GetUserPermissions { address, user } => {
                self.get_user_permissions(*address, *user, msg_id, &origin)
            }
            GetPermissions(address) => self.get_permissions(*address, msg_id, &origin),
        }
    }

    pub(super) fn write(
        &mut self,
        write: SequenceWrite,
        msg_id: MessageId,
        origin: &MsgSender,
    ) -> Option<OutboundMsg> {
        use SequenceWrite::*;
        match write {
            New(data) => self.store(&data, msg_id, origin),
            Edit(operation) => self.edit(operation, msg_id, origin),
            Delete(address) => self.delete(address, msg_id, origin),
            SetOwner(operation) => self.set_owner(operation, msg_id, origin),
            SetPubPermissions(operation) => self.set_public_permissions(operation, msg_id, origin),
            SetPrivPermissions(operation) => {
                self.set_private_permissions(operation, msg_id, origin)
            }
        }
    }

    fn store(
        &mut self,
        data: &SData,
        msg_id: MessageId,
        origin: &MsgSender,
    ) -> Option<OutboundMsg> {
        let result = if self.chunks.has(data.address()) {
            Err(NdError::DataExists)
        } else {
            self.chunks
                .put(&data)
                .map_err(|error| error.to_string().into())
        };
        self.ok_or_error(result, msg_id, &origin)
    }

    fn get(
        &self,
        address: SDataAddress,
        msg_id: MessageId,
        origin: &MsgSender,
    ) -> Option<OutboundMsg> {
        let result = self.get_chunk(address, SDataAction::Read, origin);
        self.decisions.send(Message::QueryResponse {
            response: QueryResponse::GetSequence(result),
            id: MessageId::new(),
            query_origin: origin.address(),
            correlation_id: msg_id,
        })
    }

    fn get_chunk(
        &self,
        address: SDataAddress,
        action: SDataAction,
        origin: &MsgSender,
    ) -> Result<SData, NdError> {
        //let requester_key = utils::own_key(requester).ok_or(NdError::AccessDenied)?;
        let data = self.chunks.get(&address).map_err(|error| match error {
            ChunkStoreError::NoSuchChunk => NdError::NoSuchData,
            _ => error.to_string().into(),
        })?;
        data.check_permission(action, *origin.id())?;
        Ok(data)
    }

    fn delete(
        &mut self,
        address: SDataAddress,
        msg_id: MessageId,
        origin: &MsgSender,
    ) -> Option<OutboundMsg> {
        //let requester_pk = *utils::own_key(&requester)?;
        let result = self
            .chunks
            .get(&address)
            .map_err(|error| match error {
                ChunkStoreError::NoSuchChunk => NdError::NoSuchData,
                error => error.to_string().into(),
            })
            .and_then(|sdata| {
                // TODO - SData::check_permission() doesn't support Delete yet in safe-nd
                if sdata.address().is_pub() {
                    Err(NdError::InvalidOperation)
                } else {
                    sdata.check_is_last_owner(*origin.id())
                }
            })
            .and_then(|_| {
                self.chunks
                    .delete(&address)
                    .map_err(|error| error.to_string().into())
            });

        self.ok_or_error(result, msg_id, &origin)
    }

    fn get_range(
        &self,
        address: SDataAddress,
        range: (SDataIndex, SDataIndex),
        msg_id: MessageId,
        origin: &MsgSender,
    ) -> Option<OutboundMsg> {
        let result = self
            .get_chunk(address, SDataAction::Read, origin)
            .and_then(|sdata| sdata.in_range(range.0, range.1).ok_or(NdError::NoSuchEntry));
        self.decisions.send(Message::QueryResponse {
            response: QueryResponse::GetSequenceRange(result),
            id: MessageId::new(),
            query_origin: origin.address(),
            correlation_id: msg_id,
        })
    }

    fn get_last_entry(
        &self,
        address: SDataAddress,
        msg_id: MessageId,
        origin: &MsgSender,
    ) -> Option<OutboundMsg> {
        let result =
            self.get_chunk(address, SDataAction::Read, origin)
                .and_then(|sdata| match sdata.last_entry() {
                    Some(entry) => Ok((sdata.entries_index() - 1, entry.to_vec())),
                    None => Err(NdError::NoSuchEntry),
                });
        self.decisions.send(Message::QueryResponse {
            response: QueryResponse::GetSequenceLastEntry(result),
            id: MessageId::new(),
            query_origin: origin.address(),
            correlation_id: msg_id,
        })
    }

    fn get_owner(
        &self,
        address: SDataAddress,
        msg_id: MessageId,
        origin: &MsgSender,
    ) -> Option<OutboundMsg> {
        let result = self
            .get_chunk(address, SDataAction::Read, origin)
            .and_then(|sdata| {
                let index = sdata.owners_index() - 1;
                sdata.owner(index).cloned().ok_or(NdError::InvalidOwners)
            });
        self.decisions.send(Message::QueryResponse {
            response: QueryResponse::GetSequenceOwner(result),
            id: MessageId::new(),
            query_origin: origin.address(),
            correlation_id: msg_id,
        })
    }

    fn get_user_permissions(
        &self,
        address: SDataAddress,
        user: SDataUser,
        msg_id: MessageId,
        origin: &MsgSender,
    ) -> Option<OutboundMsg> {
        let result = self
            .get_chunk(address, SDataAction::Read, origin)
            .and_then(|sdata| {
                let index = sdata.permissions_index() - 1;
                sdata.user_permissions(user, index)
            });
        self.decisions.send(Message::QueryResponse {
            response: QueryResponse::GetSequenceUserPermissions(result),
            id: MessageId::new(),
            query_origin: origin.address(),
            correlation_id: msg_id,
        })
    }

    fn get_permissions(
        &self,
        address: SDataAddress,
        msg_id: MessageId,
        origin: &MsgSender,
    ) -> Option<OutboundMsg> {
        let result = self
            .get_chunk(address, SDataAction::Read, origin)
            .and_then(|sdata| {
                let index = sdata.permissions_index() - 1;
                let res = if sdata.is_pub() {
                    SDataPermissions::from(sdata.pub_permissions(index)?.clone())
                } else {
                    SDataPermissions::from(sdata.priv_permissions(index)?.clone())
                };
                Ok(res)
            });
        self.decisions.send(Message::QueryResponse {
            response: QueryResponse::GetSequencePermissions(result),
            id: MessageId::new(),
            query_origin: origin.address(),
            correlation_id: msg_id,
        })
    }

    fn set_public_permissions(
        &mut self,
        write_op: SDataWriteOp<SDataPubPermissions>,
        msg_id: MessageId,
        origin: &MsgSender,
    ) -> Option<OutboundMsg> {
        let address = write_op.address;
        let result = self.edit_chunk(
            address,
            SDataAction::ManagePermissions,
            origin,
            move |mut sdata| {
                sdata.apply_crdt_pub_perms_op(write_op.crdt_op)?;
                Ok(sdata)
            },
        );
        self.ok_or_error(result, msg_id, &origin)
    }

    fn set_private_permissions(
        &mut self,
        write_op: SDataWriteOp<SDataPrivPermissions>,
        msg_id: MessageId,
        origin: &MsgSender,
    ) -> Option<OutboundMsg> {
        let address = write_op.address;
        let result = self.edit_chunk(
            address,
            SDataAction::ManagePermissions,
            origin,
            move |mut sdata| {
                sdata.apply_crdt_priv_perms_op(write_op.crdt_op)?;
                Ok(sdata)
            },
        );
        self.ok_or_error(result, msg_id, origin)
    }

    fn set_owner(
        &mut self,
        write_op: SDataWriteOp<SDataOwner>,
        msg_id: MessageId,
        origin: &MsgSender,
    ) -> Option<OutboundMsg> {
        let address = write_op.address;
        let result = self.edit_chunk(
            address,
            SDataAction::ManagePermissions,
            origin,
            move |mut sdata| {
                sdata.apply_crdt_owner_op(write_op.crdt_op);
                Ok(sdata)
            },
        );
        self.ok_or_error(result, msg_id, &origin)
    }

    fn edit(
        &mut self,
        write_op: SDataWriteOp<SDataEntry>,
        msg_id: MessageId,
        origin: &MsgSender,
    ) -> Option<OutboundMsg> {
        let address = write_op.address;
        let result = self.edit_chunk(address, SDataAction::Append, origin, move |mut sdata| {
            sdata.apply_crdt_op(write_op.crdt_op);
            Ok(sdata)
        });
        self.ok_or_error(result, msg_id, origin)
    }

    fn edit_chunk<F>(
        &mut self,
        address: SDataAddress,
        action: SDataAction,
        origin: &MsgSender,
        write_fn: F,
    ) -> NdResult<()>
    where
        F: FnOnce(SData) -> NdResult<SData>,
    {
        self.get_chunk(address, action, origin)
            .and_then(write_fn)
            .and_then(move |sdata| {
                self.chunks
                    .put(&sdata)
                    .map_err(|error| error.to_string().into())
            })
    }

    fn ok_or_error<T>(
        &self,
        result: NdResult<T>,
        msg_id: MessageId,
        origin: &MsgSender,
    ) -> Option<OutboundMsg> {
        let error = match result {
            Ok(_) => return None,
            Err(error) => error,
        };
        self.decisions.send(Message::CmdError {
            id: MessageId::new(),
            error: CmdError::Data(error),
            correlation_id: msg_id,
            cmd_origin: origin.address(),
        })
    }
}

impl Display for SequenceStorage {
    fn fmt(&self, formatter: &mut Formatter) -> fmt::Result {
        write!(formatter, "{}", self.keys.public_key())
    }
}
