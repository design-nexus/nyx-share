//! Print the service's events as they happen: `cargo run --example events`.

fn main() -> Result<(), nyx_share_client::Error> {
    let client = nyx_share_client::Client::connect()?;
    for event in client.events()? {
        println!("{event:?}");
    }
    Ok(())
}
