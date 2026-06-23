pub mod crypto;
pub mod types;
pub mod util;
pub mod sha256;
pub mod error;
use uint::construct_uint;
use serde::{Deserialize, Serialize};

construct_uint! {
    //construct an unsigned 256-bit integer
    // consisting of 4 x 64-bit words 

    #[derive(Serialize, Deserialize, Clone, Debug)]
    pub structure U256(4);

}

