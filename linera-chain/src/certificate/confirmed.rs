// Copyright (c) Facebook, Inc. and its affiliates.
// Copyright (c) Zefchain Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use linera_base::{
    crypto::{ValidatorPublicKey, ValidatorSignature},
    data_types::{Epoch, Round},
    identifiers::{ChainId, MessageId},
};
use serde::{ser::SerializeStruct, Deserialize, Deserializer, Serialize};

use super::{generic::GenericCertificate, Certificate};
use crate::{
    block::{Block, ConfirmedBlock, ConversionError},
    data_types::{Medium, MessageBundle},
};

impl GenericCertificate<ConfirmedBlock> {
    /// Returns reference to the `Block` contained in this certificate.
    pub fn block(&self) -> &Block {
        self.inner().block()
    }

    /// Returns whether this value contains the message with the specified ID.
    pub fn has_message(&self, message_id: &MessageId) -> bool {
        self.block().message_by_id(message_id).is_some()
    }

    /// Returns the bundles of messages sent via the given medium to the specified
    /// recipient. Messages originating from different transactions of the original block
    /// are kept in separate bundles. If the medium is a channel, does not verify that the
    /// recipient is actually subscribed to that channel.
    pub fn message_bundles_for<'a>(
        &'a self,
        medium: &'a Medium,
        recipient: ChainId,
    ) -> impl Iterator<Item = (Epoch, MessageBundle)> + 'a {
        let certificate_hash = self.hash();
        self.block()
            .message_bundles_for(medium, recipient, certificate_hash)
    }

    #[cfg(with_testing)]
    pub fn outgoing_message_count(&self) -> usize {
        self.block().messages().iter().map(Vec::len).sum()
    }
}

impl TryFrom<Certificate> for GenericCertificate<ConfirmedBlock> {
    type Error = ConversionError;

    fn try_from(cert: Certificate) -> Result<Self, Self::Error> {
        match cert {
            Certificate::Confirmed(confirmed) => Ok(confirmed),
            _ => Err(ConversionError::ConfirmedBlock),
        }
    }
}

impl From<GenericCertificate<ConfirmedBlock>> for Certificate {
    fn from(cert: GenericCertificate<ConfirmedBlock>) -> Certificate {
        Certificate::Confirmed(cert)
    }
}

impl Serialize for GenericCertificate<ConfirmedBlock> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("ConfirmedBlockCertificate", 3)?;
        state.serialize_field("value", self.inner())?;
        state.serialize_field("round", &self.round)?;
        state.serialize_field("signatures", self.signatures())?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for GenericCertificate<ConfirmedBlock> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Debug, Deserialize)]
        #[serde(rename = "ConfirmedBlockCertificate")]
        struct Helper {
            value: ConfirmedBlock,
            round: Round,
            signatures: Vec<(ValidatorPublicKey, ValidatorSignature)>,
        }

        let helper = Helper::deserialize(deserializer)?;
        if !crate::data_types::is_strictly_ordered(&helper.signatures) {
            Err(serde::de::Error::custom("Vector is not strictly sorted"))
        } else {
            Ok(Self::new(helper.value, helper.round, helper.signatures))
        }
    }
}
