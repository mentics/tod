//! Outline entity repositories.

pub mod gate;
pub mod list;
pub mod node;
pub mod obligations;
pub mod outline;
pub mod tree;

pub use gate::GateRepo;
pub use list::ListRepo;
pub use node::NodeRepo;
pub use obligations::ObligationRepo;
pub use outline::OutlineRepo;
pub use tree::TreeLoader;
