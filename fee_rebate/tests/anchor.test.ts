describe("fee rebate Program Tests", () => {
  // Global variables to share state across tests
  let marketStateKeypair;
  let makerUserPda, makerUserBump;
  let takerUserPda, takerUserBump;

  // Create distinct Keypairs for Maker and Taker authorities
  // so they can sign instructions on their behalf.
  const makerAuthority = web3.Keypair.generate();
  const takerAuthority = web3.Keypair.generate();

  // Test: Initialize Market
  it("Initialize Market", async () => {
    // Create a Keypair for the MarketState account.
    marketStateKeypair = web3.Keypair.generate();

    // Define the chosen fee parameters
    const makerRebateBps = 2;
    const takerFeeBps = 5;
    const referralBps = 1;

    // Fire the transaction
    const txHash = await pg.program.methods
      .initializeMarket(makerRebateBps, takerFeeBps, referralBps)
      .accounts({
        marketState: marketStateKeypair.publicKey,
        authority: pg.wallet.publicKey, // The admin authority is pg.wallet
        systemProgram: web3.SystemProgram.programId,
      })
      .signers([marketStateKeypair]) // new account creation requires the Keypair
      .rpc();

    console.log("initializeMarket tx:", txHash);
    // Optionally confirm the transaction (in Playground, this may be automatic)
    await pg.connection.confirmTransaction(txHash);

    // Fetch the on-chain data
    const marketState = await pg.program.account.marketState.fetch(
      marketStateKeypair.publicKey
    );

    console.log("Market State:", marketState);
    // Some basic assertions
    assert.equal(marketState.makerRebateBps, makerRebateBps);
    assert.equal(marketState.takerFeeBps, takerFeeBps);
    assert.equal(marketState.referralBps, referralBps);
  });

  // 2) Test: Register Maker User
  it("Register Maker User", async () => {
    // Derive the Maker userState PDA using the same seeds as in lib.rs
    [makerUserPda, makerUserBump] = await web3.PublicKey.findProgramAddress(
      [
        Buffer.from("user_state"),
        makerAuthority.publicKey.toBuffer(),
      ],
      pg.program.programId
    );

    // The instruction’s `referrer: Option<Pubkey>` can be null (None) for the Maker
    const referrer = null;

    const txHash = await pg.program.methods
      .registerUser(referrer) // pass null => Option<Pubkey>::None
      .accounts({
        userState: makerUserPda,
        userAuthority: makerAuthority.publicKey, // The Maker must sign
        systemProgram: web3.SystemProgram.programId,
      })
      .signers([makerAuthority]) // The userAuthority must sign
      .rpc();

    console.log("registerUser (Maker) tx:", txHash);
    await pg.connection.confirmTransaction(txHash);

    const makerUserState = await pg.program.account.userState.fetch(makerUserPda);
    console.log("Maker User State:", makerUserState);

    // Basic assertion
    assert.ok(makerUserState.authority.equals(makerAuthority.publicKey));
    assert.equal(makerUserState.referrer, null);
  });

  // Test: Register Taker User (with Maker as a referrer)
  it("Register Taker User", async () => {
    // Derive Taker userState PDA
    [takerUserPda, takerUserBump] = await web3.PublicKey.findProgramAddress(
      [
        Buffer.from("user_state"),
        takerAuthority.publicKey.toBuffer(),
      ],
      pg.program.programId
    );

    // Set the Maker as the Taker’s referrer
    const referrer = makerAuthority.publicKey; // Option<Pubkey>::Some(<maker>)
    
    const txHash = await pg.program.methods
      .registerUser(referrer)
      .accounts({
        userState: takerUserPda,
        userAuthority: takerAuthority.publicKey,
        systemProgram: web3.SystemProgram.programId,
      })
      .signers([takerAuthority])
      .rpc();

    console.log("registerUser (Taker) tx:", txHash);
    await pg.connection.confirmTransaction(txHash);

    const takerUserState = await pg.program.account.userState.fetch(takerUserPda);
    console.log("Taker User State:", takerUserState);

    assert.ok(takerUserState.authority.equals(takerAuthority.publicKey));
    // Referrer should be stored
    assert.ok(takerUserState.referrer && takerUserState.referrer.equals(makerAuthority.publicKey));
  });

  // 4) Test: Place an order as Maker
  it("Place Order as Maker", async () => {
    // Place a simple sell (Ask) order with price=100, size=10, no expiry
    const price = new BN(100);
    const size = new BN(10);
    const expiryTimestamp = new BN(0); // 0 => no expiry

    const txHash = await pg.program.methods
      .placeOrder({ bid: {} }, price, size, expiryTimestamp)
      .accounts({
        userState: makerUserPda,
        userAuthority: makerAuthority.publicKey,
      })
      .signers([makerAuthority])
      .rpc();

    console.log("placeOrder tx:", txHash);
    await pg.connection.confirmTransaction(txHash);

    // Check the maker's orders array
    const makerUserState = await pg.program.account.userState.fetch(makerUserPda);
    console.log("Maker User State (after placeOrder):", makerUserState);

    const [firstOrder] = makerUserState.orders;
    // Adjust to ensure correct usage of enum for OrderSide
    assert.equal(firstOrder.sizeRemaining.toString(), "10");
    assert.equal(firstOrder.price.toString(), "100");
  });

  // 5) Test: Fill the Maker’s Order as Taker
  it("Fill Order", async () => {
    // Fill 5 out of 10
    const makerOrderIndex = 0;
    const fillSize = new BN(5);

    // Fetch the necessary data from previous steps
    const txHash = await pg.program.methods
      .fillOrder(makerOrderIndex, fillSize)
      .accounts({
        marketState: marketStateKeypair.publicKey,
        makerUser: makerUserPda,
        takerUser: takerUserPda,
        takerAuthority: takerAuthority.publicKey,
      })
      .signers([takerAuthority]) // Taker must sign
      .rpc();

    console.log("fillOrder tx:", txHash);
    await pg.connection.confirmTransaction(txHash);

    // Fetch updated user states
    const makerUserState = await pg.program.account.userState.fetch(makerUserPda);
    const takerUserState = await pg.program.account.userState.fetch(takerUserPda);
    const marketState = await pg.program.account.marketState.fetch(
      marketStateKeypair.publicKey
    );

    // Maker's first order should now have size_remaining=5 (partial fill)
    const [firstOrder] = makerUserState.orders;
    assert.equal(firstOrder.sizeRemaining.toString(), "5");

    console.log("Maker stats:", {
      makerVolume: makerUserState.makerVolume.toString(),
      makerRebatesEarned: makerUserState.makerRebatesEarned.toString(),
    });
    console.log("Taker stats:", {
      takerVolume: takerUserState.takerVolume.toString(),
      takerFeesPaid: takerUserState.takerFeesPaid.toString(),
    });
    console.log("Market fees collected:", marketState.totalFeesCollected.toString());

    // Simple checks
    assert.equal(makerUserState.makerVolume.toString(), "5"); // makerVolume increments by fillSize
    assert.equal(takerUserState.takerVolume.toString(), "5");
    // Ensure some fee was collected
    assert.ok(marketState.totalFeesCollected.gtn(0));
  });

  //  Test: Withdraw Fees (optional)
  it("Withdraw Fees", async () => {
    // Withdraw 1 lamport from the collected fees
    const withdrawAmount = new BN(1);

    const txHash = await pg.program.methods
      .withdrawFees(withdrawAmount)
      .accounts({
        marketState: marketStateKeypair.publicKey,
        authority: pg.wallet.publicKey, // Must match market_state.authority
      })
      .rpc();

    console.log("withdrawFees tx:", txHash);
    await pg.connection.confirmTransaction(txHash);

    // Check new fees
    const marketState = await pg.program.account.marketState.fetch(
      marketStateKeypair.publicKey
    );
    console.log("Market fees after withdrawal:", marketState.totalFeesCollected.toString());

    // Ensure the fees have decreased by the withdraw amount
    // (Compare old vs new fees as needed)
  });
});
