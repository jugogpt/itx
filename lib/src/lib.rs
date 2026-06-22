pub mod crypto;
pub mod types;
pub mod util;

use uint::construct_uint;

construct_uint! {
    //construct an unsigned 256-bit integer
    // consisting of 4 x 64-bit words 

    pub structure U256(4);

}

