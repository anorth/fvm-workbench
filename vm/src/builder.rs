use fvm_ipld_blockstore::Blockstore;
use fvm::state_tree::{ActorState, StateTree};
use fvm_shared::address::Address;
use cid::Cid;
use fvm_shared::econ::TokenAmount;
use multihash::Code;
use fvm_ipld_car::load_car_unchecked;
use fvm::externs::Externs;
use fvm_shared::version::NetworkVersion;
use fvm_shared::state::StateTreeVersion;
use fvm::machine::{DefaultMachine, Engine, MachineContext, Manifest, NetworkConfig};
use fvm_shared::ActorID;
use fvm::{account_actor, DefaultKernel, init_actor, system_actor};
use fvm_ipld_hamt::Hamt;
use fvm::executor::DefaultExecutor;
use fvm::call_manager::DefaultCallManager;
use fvm_ipld_encoding::CborStore;
use anyhow::{anyhow, Context};
use futures::executor::block_on;
use fvm_ipld_encoding::ser::Serialize;
use fvm_shared::bigint::Zero;
use crate::Bench;

// A workbench backed by a real FVM instance.
// TODO:
// - convenience setters for base fee, supply etc, abstract the Message
// - ability to set a custom manifest

/// A factory for workbench instances.
/// Built-in actors must be installed before the workbench is ready for use,
/// due to limitations of the underlying Machine (it won't observe state tree mutations
/// made externally).
/// Code for built-in actors may be loaded from either a bundle or a manifest, before
/// actors can then be constructed.
pub struct BenchBuilder<B, E>
where
    B: Blockstore + Clone + 'static,
    E: Externs + Clone + 'static,
{
    externs: E,
    machine_ctx: MachineContext,
    state_tree: StateTree<B>,
    builtin_manifest_data_cid: Option<Cid>,
    builtin_manifest: Option<Manifest>,
}

// These built-in actor types are defined in the built-in actors repo (which is not imported here)
// and are used as the sequence of actors codes in the manifest.
// We could replace these with name strings if the FVM Manifest provided access to them.
const SYSTEM_ACTOR_TYPE_ID: u32 = 1;
const INIT_ACTOR_TYPE_ID: u32 = 2;
const ACCOUNT_ACTOR_TYPE_ID: u32 = 4;

impl<B, E> BenchBuilder<B, E>
where
    B: Blockstore + Clone,
    E: Externs + Clone,
{
    /// Create a new BenchBuilder and loads built-in actor code from a bundle.
    pub fn new_with_bundle(
        blockstore: B,
        externs: E,
        nv: NetworkVersion,
        state_tree_version: StateTreeVersion,
        builtin_bundle: &[u8],
    ) -> anyhow::Result<Self> {
        let mut bb = BenchBuilder::new_bare(blockstore, externs, nv, state_tree_version)?;
        bb.install_builtin_actor_bundle(builtin_bundle)?;
        Ok(bb)
    }

    /// Creates a new BenchBuilder with no installed code for built-in actors.
    pub fn new_bare(
        blockstore: B,
        externs: E,
        nv: NetworkVersion,
        state_tree_version: StateTreeVersion,
    ) -> anyhow::Result<Self> {
        let network_conf = NetworkConfig::new(nv);
        // network_conf.enable_actor_debugging(); // This doesn't seem to do anything.
        let machine_ctx = MachineContext {
            network: network_conf,
            epoch: 0,
            // timestamp: 0, // For FVM v3
            base_fee: TokenAmount::from_atto(100),
            initial_state_root: Default::default(),
            circ_supply: TokenAmount::from_whole(1_000_000),
            tracing: true,
        };
        let state_tree =
            StateTree::new(blockstore.clone(), state_tree_version).map_err(anyhow::Error::from)?;

        Ok(Self {
            externs,
            machine_ctx,
            state_tree,
            builtin_manifest_data_cid: None,
            builtin_manifest: None,
        })
    }

    pub fn store(&self) -> &B {
        self.state_tree.store()
    }

    /// Imports built-in actor code and manifest into the state tree from a bundle in CAR format.
    /// After this, built-in actors can be created from the code thus installed.
    /// Does not create any actors.
    pub fn install_builtin_actor_bundle(&mut self, bundle_data: &[u8]) -> anyhow::Result<()> {
        if self.builtin_manifest.is_some() {
            return Err(anyhow!("built-in actors already installed"));
        }
        let store = self.state_tree.store();
        let bundle_root = import_bundle(store, bundle_data).unwrap();

        let (manifest_version, manifest_data_cid): (u32, Cid) = match store
            .get_cbor(&bundle_root)?
        {
            Some((manifest_version, manifest_data)) => (manifest_version, manifest_data),
            None => return Err(anyhow!("no manifest information in bundle root {}", bundle_root)),
        };
        self.builtin_manifest_data_cid = Some(manifest_data_cid);
        self.builtin_manifest = Some(Manifest::load(store, &manifest_data_cid, manifest_version)?);
        Ok(())
    }

    /// Installs built-in actors code from a manifest provided directly.
    pub fn install_builtin_manifest(&mut self, _manifest: &Manifest) -> anyhow::Result<()> {
        // Write manifest data to blockstore
        // Set local manifest data cid
        // Caller will also need to install the actor code for each actor in the manifest
        todo!()
    }

    /// Creates the System and Init actors using code specified in the manifest.
    /// These actors must be installed before the executor can be built or used.
    pub fn create_system_actors(&mut self) -> anyhow::Result<()> {
        self.create_system_actor()?;
        self.create_init_actor()?;
        Ok(())
    }

    /// Creates a singleton built-in actor using code specified in the manifest.
    /// A singleton actor does not have a robust/key address resolved via the Init actor.
    pub fn create_singleton_actor(
        &mut self,
        type_id: u32,
        address: &Address,
        state: &impl Serialize,
        balance: TokenAmount,
    ) -> anyhow::Result<()> {
        self.create_builtin_actor_internal(type_id, address, state, balance)
    }

    /// Creates a non-singleton built-in actor using code specified in the manifest.
    /// Returns the assigned ActorID.
    pub fn create_builtin_actor(
        &mut self,
        type_id: u32,
        address: &Address,
        state: &impl Serialize,
        balance: TokenAmount,
    ) -> anyhow::Result<ActorID> {
        // It would be nice to be able to use the VM to execute the actor's constructor,
        // but the VM isn't ready yet.
        // Establish the address mapping in Init actor.
        let mut init_actor = self
            .state_tree
            .get_actor_id(init_actor::INIT_ACTOR_ADDR.id().unwrap() as ActorID)?
            .unwrap();
        let mut init_state: init_actor::State =
            self.store().get_cbor(&init_actor.state).unwrap().unwrap();
        let new_id = init_state.map_address_to_new_id(self.store(), &address)?;
        let state_cid = self
            .state_tree
            .store()
            .put_cbor(&init_state, Code::Blake2b256)
            .context("failed to put actor state while updating")
            .unwrap();
        init_actor.state = state_cid;
        self.state_tree.set_actor(&init_actor::INIT_ACTOR_ADDR, init_actor).unwrap();

        // Create the actor.
        self.create_builtin_actor_internal(
            type_id,
            &Address::new_id(new_id),
            &state,
            balance,
        )?;
        Ok(new_id)
    }

    /// Creates the system actor, using code specified in the manifest.
    pub fn create_system_actor(&mut self) -> anyhow::Result<()> {
        if self.builtin_manifest_data_cid.is_none() {
            return Err(anyhow!("built-in actor bundle not loaded"));
        }
        // Note: the FVM Tester incorrectly sets the bundle root CID in system actor state here,
        // but it should be the manifest data CID.
        // The error is masked by also providing a builtin-actors override.
        let state = system_actor::State { builtin_actors: self.builtin_manifest_data_cid.unwrap() };
        self.create_builtin_actor_internal(
            SYSTEM_ACTOR_TYPE_ID,
            &system_actor::SYSTEM_ACTOR_ADDR,
            &state,
            TokenAmount::zero(),
        )
    }

    /// Creates the init actor, using code specified in the manifest.
    pub fn create_init_actor(&mut self) -> anyhow::Result<()> {
        let e_cid =
            Hamt::<_, String>::new_with_bit_width(self.state_tree.store(), 5).flush().unwrap();
        let state = init_actor::State {
            address_map: e_cid,
            next_id: 100,
            network_name: "bench".to_string(),
        };
        self.create_builtin_actor_internal(
            INIT_ACTOR_TYPE_ID,
            &init_actor::INIT_ACTOR_ADDR,
            &state,
            TokenAmount::zero(),
        )
    }

    // The system actor must be installed before the workbench can be built.
    // It's not necessary to install any other actors, though the Init actor must also be
    // installed in order to subsequently send any message.
    pub fn build(&mut self) -> anyhow::Result<Bench<B, E>> {
        self.create_system_actor()?;
        self.create_init_actor()?;

        // Clone the context so the builder can be re-used for a new bench.
        let mut machine_ctx = self.machine_ctx.clone();

        // Flush the state tree to store and calculate the initial root.
        let state_root = self.state_tree.flush().map_err(anyhow::Error::from)?;
        machine_ctx.initial_state_root = state_root;

        let engine_conf = (&machine_ctx.network).into();
        let machine = DefaultMachine::new(
            &Engine::new_default(engine_conf)?,
            &machine_ctx,
            self.state_tree.store().clone(),
            self.externs.clone(),
        )?;
        let executor =
            DefaultExecutor::<DefaultKernel<DefaultCallManager<DefaultMachine<B, E>>>>::new(
                machine,
            );
        Ok(Bench::new(executor))
    }

    ///// Private helpers /////

    fn create_builtin_actor_internal(
        &mut self,
        type_id: u32,
        addr: &Address,
        state: &impl Serialize,
        balance: TokenAmount,
    ) -> anyhow::Result<()> {
        if let Some(manifest) = self.builtin_manifest.as_ref() {
            let code_cid = manifest.code_by_id(type_id).unwrap();
            create_actor(&mut self.state_tree, addr, *code_cid, state, balance)
        } else {
            Err(anyhow!("built-in actor manifest not loaded"))
        }
    }
}

fn import_bundle(blockstore: &impl Blockstore, bundle: &[u8]) -> anyhow::Result<Cid> {
    match &*block_on(async { load_car_unchecked(blockstore, bundle).await })? {
        [root] => Ok(*root),
        _ => Err(anyhow!("multiple root CIDs in bundle")),
    }
}

fn create_actor<B: Blockstore>(
    state_tree: &mut StateTree<B>,
    id_addr: &Address,
    code: Cid,
    state: &impl Serialize,
    balance: TokenAmount,
) -> anyhow::Result<()> {
    let state_cid = state_tree
        .store()
        .put_cbor(state, Code::Blake2b256)
        .context("failed to put actor state while installing")?;

    let actor_state = ActorState { code, state: state_cid, sequence: 0, balance };
    state_tree
        .set_actor(id_addr, actor_state)
        .map_err(anyhow::Error::from)
        .context("failed to install actor")
}