use fil_actors_runtime::INIT_ACTOR_ADDR;
use fvm::trace::ExecutionTrace;
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::bigint::Zero;
use fvm_shared::econ::TokenAmount;
use fvm_shared::error::ExitCode;
use fvm_shared::message::Message;
use fvm_shared::state::StateTreeVersion;
use fvm_shared::version::NetworkVersion;
use fvm_shared::{BLOCK_GAS_LIMIT, METHOD_SEND};
use fvm_workbench_builtin_actors::genesis::{create_genesis_actors, GenesisSpec};
use fvm_workbench_vm::Bench;
use fvm_workbench_vm::{BenchBuilder, ExecutionWrangler, FakeExterns};

#[test]
fn test_hookup() {
    let (mut builder, manifest_data_cid) = BenchBuilder::new_with_bundle(
        MemoryBlockstore::new(),
        FakeExterns::new(),
        NetworkVersion::V16,
        StateTreeVersion::V4,
        actors_v10::BUNDLE_CAR,
    )
    .unwrap();

    let spec = GenesisSpec::default(manifest_data_cid);
    let genesis = create_genesis_actors(&mut builder, &spec).unwrap();
    let mut bench = builder.build().unwrap();

    let mut wrangler = ExecutionWrangler::new_default(&mut bench);
    let ret = wrangler.execute(
        genesis.faucet_address(),
        INIT_ACTOR_ADDR.clone(),
        METHOD_SEND,
        RawBytes::default(),
        TokenAmount::zero(),
    ).unwrap();

    assert_eq!(ExitCode::OK, ret.receipt.exit_code);
    println!("trace: {:?}", format_trace(&ret.trace));
}

fn format_trace(trace: &ExecutionTrace) {
    for event in trace {
        println!("event: {:?}", event);
    }
}
