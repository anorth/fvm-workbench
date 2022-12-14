use crate::trace::ExecutionTrace;
use cid::Cid;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::ser::Serialize;
use fvm_shared::address::Address;
use fvm_shared::clock::ChainEpoch;
use fvm_shared::econ::TokenAmount;
use fvm_shared::message::Message;
use fvm_shared::receipt::Receipt;
use fvm_shared::ActorID;

pub mod trace;
pub mod wrangler;

/// A factory for workbench instances.
/// Built-in actors must be installed before the workbench can be created.
// TODO: Configuration of default circulating supply, base fee etc.
pub trait WorkbenchBuilder {
    type B: Blockstore;

    /// Returns a reference to the blockstore underlying this builder.
    fn store(&self) -> &Self::B;

    /// Creates a singleton built-in actor using code specified in the manifest.
    /// A singleton actor does not have a robust/key address resolved via the Init actor.
    fn create_singleton_actor(
        &mut self,
        type_id: u32,
        id: ActorID,
        state: &impl Serialize,
        balance: TokenAmount,
    ) -> anyhow::Result<()>;

    /// Creates a non-singleton built-in actor using code specified in the manifest.
    /// Returns the assigned ActorID.
    fn create_builtin_actor(
        &mut self,
        type_id: u32,
        address: &Address,
        state: &impl Serialize,
        balance: TokenAmount,
    ) -> anyhow::Result<ActorID>;

    /// Creates a workbench ready to execute messages.
    /// The System and Init actors must be created before a workbench can be built or used.
    fn build(&mut self) -> anyhow::Result<Box<dyn Bench>>;
}

/// A VM workbench that can execute messages to actors.
pub trait Bench {
    /// Executes a message on the workbench VM.
    /// Explicit messages increment the sender's nonce and charge for gas consumed.
    fn execute(&mut self, msg: Message, msg_length: usize) -> anyhow::Result<ExecutionResult>;
    /// Implicit messages ignore the nonce and charge no gas (but still account for it).
    fn execute_implicit(
        &mut self,
        msg: Message,
        msg_length: usize,
    ) -> anyhow::Result<ExecutionResult>;

    /// Returns the VM's current epoch.
    fn epoch(&self) -> ChainEpoch;
    /// Returns a reference to the VM's blockstore.
    fn store(&self) -> &dyn Blockstore;
    /// Looks up a top-level actor state object in the VM.
    /// Returns None if no such actor is found.
    fn find_actor(&self, id: ActorID) -> anyhow::Result<Option<ActorState>>;
    /// Resolves an address to an actor ID.
    /// Returns None if the address cannot be resolved.
    fn resolve_address(&self, addr: &Address) -> anyhow::Result<Option<ActorID>>;
}

/// The result of a message execution.
/// This duplicates a lot from an FVM-internal type, but is independent of VM.
pub struct ExecutionResult {
    /// Message receipt for the transaction.
    pub receipt: Receipt,
    /// Gas penalty from transaction, if any.
    pub penalty: TokenAmount,
    /// Tip given to miner from message.
    pub miner_tip: TokenAmount,

    // Gas tracing
    pub gas_burned: i64,
    pub base_fee_burn: TokenAmount,
    pub over_estimation_burn: TokenAmount,

    /// Execution trace information, for debugging.
    pub trace: ExecutionTrace,
    pub message: String,
}

/// An actor root state object.
pub struct ActorState {
    /// Link to code for the actor.
    pub code: Cid,
    /// Link to the state of the actor.
    pub state: Cid,
    /// Sequence of the actor.
    pub sequence: u64,
    /// Tokens available to the actor.
    pub balance: TokenAmount,
}
