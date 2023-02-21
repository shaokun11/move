use aptos_vm::{AptosVM};
use tokio::fs::File;
use tokio::io::{self, AsyncReadExt};
#[tokio::main]
async fn main() -> io::Result<()> {
    let mut f = File::open("foo.txt").await?;
    let mut buffer = String::new();
    f.read_to_string(&mut buffer).await?;
    println!("The bytes: {}", buffer);
    let count = AptosVM::get_num_proof_reading_threads();
    println!("pool count: {}", count);
    Ok(())
}