// Copyright (c) Facebook, Inc. and its affiliates.
// Copyright (c) Zefchain Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Wasm specific worker tests.
//!
//! These tests only run if a Wasm runtime has been configured by enabling either the `wasmer` or
//! the `wasmtime` feature flags.

#![allow(clippy::large_futures)]
#![cfg(any(feature = "wasmer", feature = "wasmtime"))]

use std::collections::{BTreeMap, BTreeSet};

use assert_matches::assert_matches;
use linera_base::{
    crypto::AccountSecretKey,
    data_types::{
        Amount, ApplicationDescription, Blob, BlockHeight, Bytecode, Epoch, OracleResponse,
        Timestamp,
    },
    identifiers::{ChainDescription, ChainId, ModuleId},
    ownership::ChainOwnership,
    vm::VmRuntime,
};
use linera_chain::{
    data_types::{BlockExecutionOutcome, OperationResult},
    test::{make_child_block, make_first_block, BlockTestExt},
    types::ConfirmedBlock,
};
use linera_execution::{
    system::SystemOperation, test_utils::SystemExecutionState, ExecutionRuntimeContext, Operation,
    OperationContext, ResourceController, TransactionTracker, WasmContractModule, WasmRuntime,
};
use linera_storage::{DbStorage, Storage};
#[cfg(feature = "dynamodb")]
use linera_views::dynamo_db::DynamoDbStore;
#[cfg(feature = "rocksdb")]
use linera_views::rocks_db::RocksDbStore;
#[cfg(feature = "scylladb")]
use linera_views::scylla_db::ScyllaDbStore;
use linera_views::{
    context::Context,
    memory::MemoryStore,
    views::{CryptoHashView, View},
};
use test_case::test_case;

use super::{init_worker_with_chains, make_certificate};
use crate::worker::WorkerError;

#[cfg_attr(feature = "wasmer", test_case(WasmRuntime::Wasmer ; "wasmer"))]
#[cfg_attr(feature = "wasmtime", test_case(WasmRuntime::Wasmtime ; "wasmtime"))]
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_memory_handle_certificates_to_create_application(
    wasm_runtime: WasmRuntime,
) -> anyhow::Result<()> {
    let storage = DbStorage::<MemoryStore, _>::make_test_storage(Some(wasm_runtime)).await;
    run_test_handle_certificates_to_create_application(storage, wasm_runtime).await
}

#[cfg(feature = "rocksdb")]
#[cfg_attr(feature = "wasmer", test_case(WasmRuntime::Wasmer ; "wasmer"))]
#[cfg_attr(feature = "wasmtime", test_case(WasmRuntime::Wasmtime ; "wasmtime"))]
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_rocks_db_handle_certificates_to_create_application(
    wasm_runtime: WasmRuntime,
) -> anyhow::Result<()> {
    let storage = DbStorage::<RocksDbStore, _>::make_test_storage(Some(wasm_runtime)).await;
    run_test_handle_certificates_to_create_application(storage, wasm_runtime).await
}

#[cfg(feature = "dynamodb")]
#[cfg_attr(feature = "wasmer", test_case(WasmRuntime::Wasmer ; "wasmer"))]
#[cfg_attr(feature = "wasmtime", test_case(WasmRuntime::Wasmtime ; "wasmtime"))]
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_dynamo_db_handle_certificates_to_create_application(
    wasm_runtime: WasmRuntime,
) -> anyhow::Result<()> {
    let storage = DbStorage::<DynamoDbStore, _>::make_test_storage(Some(wasm_runtime)).await;
    run_test_handle_certificates_to_create_application(storage, wasm_runtime).await
}

#[cfg(feature = "scylladb")]
#[cfg_attr(feature = "wasmer", test_case(WasmRuntime::Wasmer ; "wasmer"))]
#[cfg_attr(feature = "wasmtime", test_case(WasmRuntime::Wasmtime ; "wasmtime"))]
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_scylla_db_handle_certificates_to_create_application(
    wasm_runtime: WasmRuntime,
) -> anyhow::Result<()> {
    let storage = DbStorage::<ScyllaDbStore, _>::make_test_storage(Some(wasm_runtime)).await;
    run_test_handle_certificates_to_create_application(storage, wasm_runtime).await
}

async fn run_test_handle_certificates_to_create_application<S>(
    storage: S,
    wasm_runtime: WasmRuntime,
) -> anyhow::Result<()>
where
    S: Storage + Clone + Send + Sync + 'static,
{
    let vm_runtime = VmRuntime::Wasm;
    let admin_id = ChainDescription::Root(0);
    let publisher_owner = AccountSecretKey::generate().public().into();
    let publisher_chain = ChainDescription::Root(1);
    let creator_owner = AccountSecretKey::generate().public().into();
    let creator_chain = ChainDescription::Root(2);
    let (committee, worker) = init_worker_with_chains(
        storage.clone(),
        vec![
            (publisher_chain, publisher_owner, Amount::ZERO),
            (creator_chain, creator_owner, Amount::ZERO),
        ],
    )
    .await;

    // Load the bytecode files for a module.
    let (contract_path, service_path) =
        linera_execution::wasm_test::get_example_bytecode_paths("counter")?;
    let contract_bytecode = Bytecode::load_from_file(contract_path).await?;
    let service_bytecode = Bytecode::load_from_file(service_path).await?;

    let contract_blob = Blob::new_contract_bytecode(contract_bytecode.clone().compress());
    let service_blob = Blob::new_service_bytecode(service_bytecode.compress());

    let contract_blob_id = contract_blob.id();
    let service_blob_id = service_blob.id();

    let contract_blob_hash = contract_blob_id.hash;
    let service_blob_hash = service_blob_id.hash;

    let module_id = ModuleId::new(contract_blob_hash, service_blob_hash, vm_runtime);
    let contract = WasmContractModule::new(contract_bytecode, wasm_runtime).await?;

    // Publish the module.
    let publish_operation = SystemOperation::PublishModule { module_id };
    let publish_block = make_first_block(publisher_chain.into())
        .with_timestamp(1)
        .with_operation(publish_operation);
    let publisher_system_state = SystemExecutionState {
        committees: [(Epoch::ZERO, committee.clone())].into_iter().collect(),
        ownership: ChainOwnership::single(publisher_owner),
        timestamp: Timestamp::from(1),
        used_blobs: BTreeSet::from([contract_blob_id, service_blob_id]),
        ..SystemExecutionState::new(Epoch::ZERO, publisher_chain, admin_id)
    };
    let publisher_state_hash = publisher_system_state.clone().into_hash().await;
    let publish_block_proposal = ConfirmedBlock::new(
        BlockExecutionOutcome {
            messages: vec![Vec::new()],
            previous_message_blocks: BTreeMap::new(),
            events: vec![Vec::new()],
            blobs: vec![Vec::new()],
            state_hash: publisher_state_hash,
            oracle_responses: vec![vec![]],
            operation_results: vec![OperationResult::default()],
        }
        .with(publish_block),
    );
    let publish_certificate = make_certificate(&committee, &worker, publish_block_proposal);

    assert_matches!(
        worker
            .fully_handle_certificate_with_notifications(publish_certificate.clone(), &())
            .await,
        Err(WorkerError::BlobsNotFound(_))
    );
    storage
        .write_blobs(&[contract_blob.clone(), service_blob.clone()])
        .await?;
    let info = worker
        .fully_handle_certificate_with_notifications(publish_certificate.clone(), &())
        .await
        .unwrap()
        .info;
    assert_eq!(ChainId::from(publisher_chain), info.chain_id);
    assert_eq!(Amount::ZERO, info.chain_balance);
    assert_eq!(BlockHeight::from(1), info.next_block_height);
    assert_eq!(Timestamp::from(1), info.timestamp);
    assert_eq!(Some(publish_certificate.hash()), info.block_hash);
    assert!(info.manager.pending.is_none());

    let mut creator_system_state = SystemExecutionState {
        committees: [(Epoch::ZERO, committee.clone())].into_iter().collect(),
        ownership: ChainOwnership::single(creator_owner),
        timestamp: Timestamp::from(1),
        ..SystemExecutionState::new(Epoch::ZERO, creator_chain, admin_id)
    };

    // Create an application.
    let initial_value = 10_u64;
    let initial_value_bytes = serde_json::to_vec(&initial_value)?;
    let parameters_bytes = serde_json::to_vec(&())?;
    let create_operation = SystemOperation::CreateApplication {
        module_id,
        parameters: parameters_bytes.clone(),
        instantiation_argument: initial_value_bytes.clone(),
        required_application_ids: vec![],
    };
    let application_description = ApplicationDescription {
        module_id,
        creator_chain_id: creator_chain.into(),
        block_height: BlockHeight::from(0),
        application_index: 0,
        required_application_ids: vec![],
        parameters: parameters_bytes,
    };
    let application_description_blob = Blob::new_application_description(&application_description);
    let application_description_blob_id = application_description_blob.id();
    let application_id = From::from(&application_description);
    let create_block = make_first_block(creator_chain.into())
        .with_timestamp(2)
        .with_operation(create_operation);
    creator_system_state.timestamp = Timestamp::from(2);
    let mut creator_state = creator_system_state.into_view().await;
    creator_state
        .simulate_instantiation(
            contract.into(),
            Timestamp::from(2),
            application_description.clone(),
            initial_value_bytes.clone(),
            contract_blob,
            service_blob,
        )
        .await?;
    let create_block_proposal = ConfirmedBlock::new(
        BlockExecutionOutcome {
            messages: vec![vec![]],
            previous_message_blocks: BTreeMap::new(),
            events: vec![Vec::new()],
            state_hash: creator_state.crypto_hash().await?,
            oracle_responses: vec![vec![
                OracleResponse::Blob(contract_blob_id),
                OracleResponse::Blob(service_blob_id),
            ]],
            blobs: vec![vec![application_description_blob.clone()]],
            operation_results: vec![OperationResult::default()],
        }
        .with(create_block),
    );
    let create_certificate = make_certificate(&committee, &worker, create_block_proposal);

    storage
        .write_blobs(&[application_description_blob.clone()])
        .await?;
    creator_state
        .context()
        .extra()
        .add_blobs([application_description_blob])
        .await?;
    let info = worker
        .fully_handle_certificate_with_notifications(create_certificate.clone(), &())
        .await
        .unwrap()
        .info;
    assert_eq!(ChainId::root(2), info.chain_id);
    assert_eq!(Amount::ZERO, info.chain_balance);
    assert_eq!(BlockHeight::from(1), info.next_block_height);
    assert_eq!(Timestamp::from(2), info.timestamp);
    assert_eq!(Some(create_certificate.hash()), info.block_hash);
    assert!(info.manager.pending.is_none());

    // Execute an application operation
    let increment = 5_u64;
    let user_operation = bcs::to_bytes(&increment)?;
    let run_block = make_child_block(&create_certificate.into_value())
        .with_timestamp(3)
        .with_operation(Operation::User {
            application_id,
            bytes: user_operation.clone(),
        });
    let operation_context = OperationContext {
        chain_id: creator_chain.into(),
        authenticated_signer: None,
        authenticated_caller_id: None,
        height: run_block.height,
        round: Some(0),
    };
    let mut controller = ResourceController::default();
    creator_state
        .execute_operation(
            operation_context,
            Operation::User {
                application_id,
                bytes: user_operation,
            },
            &mut TransactionTracker::new(
                Timestamp::from(3),
                0,
                0,
                0,
                Some(vec![OracleResponse::Blob(application_description_blob_id)]),
            ),
            &mut controller,
        )
        .await?;
    creator_state.system.timestamp.set(Timestamp::from(3));
    creator_state
        .system
        .used_blobs
        .insert(&application_description_blob_id)?;
    let run_block_proposal = ConfirmedBlock::new(
        BlockExecutionOutcome {
            messages: vec![Vec::new()],
            previous_message_blocks: BTreeMap::new(),
            events: vec![Vec::new()],
            blobs: vec![Vec::new()],
            state_hash: creator_state.crypto_hash().await?,
            oracle_responses: vec![vec![]],
            operation_results: vec![OperationResult(bcs::to_bytes(&15u64)?)],
        }
        .with(run_block),
    );
    let run_certificate = make_certificate(&committee, &worker, run_block_proposal);

    let info = worker
        .fully_handle_certificate_with_notifications(run_certificate.clone(), &())
        .await
        .unwrap()
        .info;
    assert_eq!(ChainId::root(2), info.chain_id);
    assert_eq!(Amount::ZERO, info.chain_balance);
    assert_eq!(BlockHeight::from(2), info.next_block_height);
    assert_eq!(Some(run_certificate.hash()), info.block_hash);
    assert_eq!(Timestamp::from(3), info.timestamp);
    assert!(info.manager.pending.is_none());
    Ok(())
}
