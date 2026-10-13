//! Headless tests erase only IPC registration; command bodies compile unchanged.
extern crate proc_macro;
#[proc_macro_attribute]
pub fn command(
    _: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    item
}
