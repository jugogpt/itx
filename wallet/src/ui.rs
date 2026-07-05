use crate::core::Core;
use anyhow::Result;
use cursive::event::{Event, Key};
use cursive::traits::*;
use cursive::views::{
Button, Dialog, EditView, LinearLayout, Panel, ResizedView,
TextContent, TextView,
};
use cursive::Cursive;
use std::sync::{Arc, Mutex};
use tracing::*;

//this is the idea for the UI when conceiving the wallet 
// we have two buttons, on to create a transaction and the other to exit the wallet 
//in the dialog window for creating a transction, I can choose whether I will be specifying the amount in sats, or in BTC. I will enter the name of the recipient into an input field.
//this means that i should also have a button that let s me siwitch between the two different units...
//the main screen of the wallet displays my balance in big text in BTC (ini to-days's wallets for real BTC, it is almost more practical to diplay your balance in sats, rather than a tiny fraction of a BTC)
//beyond the main screen, there are two views: left view lists the paths to my keys.
//right view lists contact added to the wallet 

//conversion function + enum to track the settings 
#[derive(Clone, Copy)]
enum Unit {
    Btc,
    Sats,
}
// Convert an amount between BTC and Satoshi units.
fn convert_amount(amount: f64 from: Unit, to: Unit) -> f64 {
    match (from, to) {
        (Unit::Btc, Unit::Sats) =>  amount * 100_000_000.0,
        (Unit::Sats, Unit::Btc) => amount/ 100_000_000.0,
        _ => amount, 
    }
} //bc we are going through floats, we are losing some precision -- use bigdecimal later to fix

/// Initialize and run the user interface.
pub fn run_ui(core: Arc<Core>,balance_content: TextContent) -> Result<()> {
    info!("Initializing UI");
    let mut siv = cursive::default();
    setup_siv(&mut siv, core.clone(), balance_content);
    info!("Starting UI event loop");
    siv.run();
    info!("UI event loop ended");
    Ok(())   
}
/// Set up the Cursive interface with all necessary components and callbacks.
fn setup_siv(siv: &mut Cursive, core: Arc<Core>, balance_content: TextContent,) {
        siv.set_autorefresh(true);
        siv.set_window_title("BTC wallet".to_string());
        siv.add_global_callback('q', |s| {
            info!("Quit command received");
            s.quit()
        });
        setup_menubar(siv, core.clone());
        setup_layout(siv, core, balance_content);
        siv.add_global_callback(Event::Key(Key::Esc), |siv| {siv.select_menubar()});
        siv.select_menubar();
    }

/// Set up the menu bar with "Send" and "Quit" options.
fn setup_menubar(siv: &mut Cursive, core: Arc<Core>) {
    // ...
}
/// Set up the main layout of the application.
fn setup_layout(siv: &mut Cursive, core: Arc<Core>, balance_content: TextContent,) {
        // ...
}

/// Create the information layout containing keys and contacts.
fn create_info_layout(core: &Arc<Core>) -> LinearLayout {
    // ...
}
/// Display the send transaction dialog.
fn show_send_transaction(s: &mut Cursive, core: Arc<Core>) {   
    // ...
}
/// Create the layout for the transaction dialog.
fn create_transaction_layout(unit: Arc<Mutex<Unit>>) -> LinearLayout {
    // ...
}
/// Create the layout for selecting the transaction unit (BTC or Sats).
fn create_unit_layout(unit: Arc<Mutex<Unit>>) -> LinearLayout {
    // ...
}
/// Switch the transaction unit between BTC and Sats.
fn switch_unit(s: &mut Cursive, unit: Arc<Mutex<Unit>>) {
    // ...
}

/// Process the send transaction request.
fn send_transaction(s: &mut Cursive, core: Arc<Core>, unit: Unit,) {
        // ...
}

/// Display a success dialog after a successful transaction.
fn show_success_dialog(s: &mut Cursive) {
    // ...
}
/// Display an error dialog when a transaction fails.
fn show_error_dialog(s: &mut Cursive, error: impl std::fmt::Display,) {
    // ...
}