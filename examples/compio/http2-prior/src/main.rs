use aioduct::CompioClient;
fn main() -> Result<(), aioduct::Error> {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = CompioClient::builder().build_local().unwrap();
        let _resp = client
            .get_local("http://localhost:8080/")?
            .h2c_prior_knowledge()
            .send()
            .await?;
        Ok(())
    })
}
