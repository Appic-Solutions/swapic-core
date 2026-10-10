// MOCK ck minter, for the local ICP demo only. It implements just the calls the settlement
// canister makes of the real ckETH minter (`withdraw_erc20`, `withdrawal_status`, with the
// real interface's types), plus `mock_*` doors the demo script drives:
// - a withdrawal burns the ckUSDC (a transfer_from into this canister, the ledger's minting
//   account) and waits for the script to release the USDC on the Ethereum fork and report
//   the transaction with `mock_finalize`;
// - a deposit the script sees through the ck helper on the fork is minted with
//   `mock_mint_deposit`, as the real minter mints after the helper's event.
import Array "mo:base/Array";
import Blob "mo:base/Blob";
import Nat8 "mo:base/Nat8";
import Nat64 "mo:base/Nat64";
import Principal "mo:base/Principal";

persistent actor class MockMinter() = this {
  type Account = { owner : Principal; subaccount : ?Blob };
  type TransferArg = {
    from_subaccount : ?Blob;
    to : Account;
    amount : Nat;
    fee : ?Nat;
    memo : ?Blob;
    created_at_time : ?Nat64;
  };
  type TransferFromArgs = {
    spender_subaccount : ?Blob;
    from : Account;
    to : Account;
    amount : Nat;
    fee : ?Nat;
    memo : ?Blob;
    created_at_time : ?Nat64;
  };
  type Ledger = actor {
    icrc1_transfer : shared TransferArg -> async { #Ok : Nat; #Err : Any };
    icrc2_transfer_from : shared TransferFromArgs -> async { #Ok : Nat; #Err : Any };
  };

  type WithdrawErc20Arg = {
    amount : Nat;
    ckerc20_ledger_id : Principal;
    recipient : Text;
    from_cketh_subaccount : ?Blob;
    from_ckerc20_subaccount : ?Blob;
  };
  type RetrieveErc20Request = { cketh_block_index : Nat; ckerc20_block_index : Nat };
  type WithdrawErc20Error = { #TemporarilyUnavailable : Text };
  type EthTransaction = { transaction_hash : Text };
  type TxFinalizedStatus = {
    #Success : { transaction_hash : Text; effective_transaction_fee : ?Nat };
    #Reimbursed : { transaction_hash : Text; reimbursed_amount : Nat; reimbursed_in_block : Nat };
    #PendingReimbursement : EthTransaction;
  };
  type WithdrawalStatus = { #Pending; #TxCreated; #TxSent : EthTransaction; #TxFinalized : TxFinalizedStatus };
  type WithdrawalDetail = {
    withdrawal_id : Nat64;
    recipient_address : Text;
    from : Principal;
    from_subaccount : ?Blob;
    token_symbol : Text;
    withdrawal_amount : Nat;
    max_transaction_fee : ?Nat;
    status : WithdrawalStatus;
  };
  type WithdrawalSearchParameter = { #ByRecipient : Text; #BySenderAccount : Account; #ByWithdrawalId : Nat64 };

  var withdrawals : [WithdrawalDetail] = [];
  var nextId : Nat64 = 1;

  public shared ({ caller }) func withdraw_erc20(arg : WithdrawErc20Arg) : async {
    #Ok : RetrieveErc20Request;
    #Err : WithdrawErc20Error;
  } {
    let ledger : Ledger = actor (Principal.toText(arg.ckerc20_ledger_id));
    // the burn: into this canister, which is the ledger's minting account
    let burned = await ledger.icrc2_transfer_from({
      spender_subaccount = null;
      from = { owner = caller; subaccount = arg.from_ckerc20_subaccount };
      to = { owner = Principal.fromActor(this); subaccount = null };
      amount = arg.amount;
      fee = null;
      memo = null;
      created_at_time = null;
    });
    switch (burned) {
      case (#Err(_)) { return #Err(#TemporarilyUnavailable("the burn was refused")) };
      case (#Ok(block)) {
        let id = nextId;
        nextId += 1;
        withdrawals := Array.append(withdrawals, [{
          withdrawal_id = id;
          recipient_address = arg.recipient;
          from = caller;
          from_subaccount = arg.from_ckerc20_subaccount;
          token_symbol = "ckUSDC";
          withdrawal_amount = arg.amount;
          max_transaction_fee = null;
          status = #Pending;
        }]);
        #Ok({ cketh_block_index = Nat64.toNat(id); ckerc20_block_index = block });
      };
    };
  };

  public query func withdrawal_status(search : WithdrawalSearchParameter) : async [WithdrawalDetail] {
    switch (search) {
      case (#ByWithdrawalId(id)) { Array.filter<WithdrawalDetail>(withdrawals, func(w) { w.withdrawal_id == id }) };
      case (#ByRecipient(r)) { Array.filter<WithdrawalDetail>(withdrawals, func(w) { w.recipient_address == r }) };
      case (#BySenderAccount(a)) { Array.filter<WithdrawalDetail>(withdrawals, func(w) { w.from == a.owner }) };
    };
  };

  // MOCK: the withdrawals the script has to release on the fork
  public query func mock_pending() : async [WithdrawalDetail] {
    Array.filter<WithdrawalDetail>(withdrawals, func(w) { switch (w.status) { case (#Pending) true; case (_) false } });
  };

  // MOCK: the script released withdrawal `id` on the fork in transaction `tx_hash`
  public shared func mock_finalize(id : Nat64, tx_hash : Text) : async () {
    withdrawals := Array.map<WithdrawalDetail, WithdrawalDetail>(withdrawals, func(w) {
      if (w.withdrawal_id == id) {
        { w with status = #TxFinalized(#Success({ transaction_hash = tx_hash; effective_transaction_fee = null })) };
      } else { w };
    });
  };

  // MOCK: mint as the real minter does after the helper's deposit event. `principal32` is
  // the helper's encoding: a length byte, the principal's bytes, zero padding.
  public shared func mock_mint_deposit(ledger_id : Principal, principal32 : Blob, subaccount : Blob, amount : Nat) : async { #Ok : Nat; #Err : Text } {
    let bytes = Blob.toArray(principal32);
    if (bytes.size() != 32) { return #Err("principal32 is not 32 bytes") };
    let len = Nat8.toNat(bytes[0]);
    if (len == 0 or len > 29) { return #Err("bad principal length") };
    let owner = Principal.fromBlob(Blob.fromArray(Array.tabulate<Nat8>(len, func(i) { bytes[i + 1] })));
    let allZero = Array.foldLeft<Nat8, Bool>(Blob.toArray(subaccount), true, func(acc, b) { acc and b == 0 });
    let ledger : Ledger = actor (Principal.toText(ledger_id));
    let minted = await ledger.icrc1_transfer({
      from_subaccount = null;
      to = { owner; subaccount = if (allZero) null else ?subaccount };
      amount;
      fee = null;
      memo = null;
      created_at_time = null;
    });
    switch (minted) { case (#Ok(b)) #Ok(b); case (#Err(_)) #Err("the mint was refused") };
  };

  // MOCK: give `to` some of the ledger's token, for the demo user's starting balance
  public shared func mock_mint(ledger_id : Principal, to : Account, amount : Nat) : async { #Ok : Nat; #Err : Text } {
    let ledger : Ledger = actor (Principal.toText(ledger_id));
    switch (await ledger.icrc1_transfer({ from_subaccount = null; to; amount; fee = null; memo = null; created_at_time = null })) {
      case (#Ok(b)) #Ok(b);
      case (#Err(_)) #Err("the mint was refused");
    };
  };
};
