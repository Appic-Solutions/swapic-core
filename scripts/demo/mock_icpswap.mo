// MOCK ICPSwap pool, for the local ICP demo only. It implements just the calls the
// settlement canister makes of a real ICPSwap `SwapPool` (`metadata`, `quote`,
// `depositFromAndSwap`, with the real interface's types), at a fixed price: one token0 is
// `rate0to1` token1 units per `rateDen` token0 units. The pool pays out of balances the
// demo script mints to it.
import Int "mo:base/Int";
import Nat "mo:base/Nat";
import Nat32 "mo:base/Nat32";
import Char "mo:base/Char";
import Principal "mo:base/Principal";
import Text "mo:base/Text";

persistent actor class MockIcpSwap(token0 : Principal, token1 : Principal, rate0to1 : Nat, rateDen : Nat) = this {
  type Account = { owner : Principal; subaccount : ?Blob };
  type Error = { #CommonError; #InternalError : Text; #UnsupportedToken : Text; #InsufficientFunds };
  type Token = { address : Text; standard : Text };
  type PoolMetadata = {
    fee : Nat;
    key : Text;
    sqrtPriceX96 : Nat;
    tick : Int;
    liquidity : Nat;
    token0 : Token;
    token1 : Token;
    maxLiquidityPerTick : Nat;
    nextPositionId : Nat;
  };
  type SwapArgs = { amountIn : Text; zeroForOne : Bool; amountOutMinimum : Text };
  type DepositAndSwapArgs = {
    tokenInFee : Nat;
    amountIn : Text;
    zeroForOne : Bool;
    amountOutMinimum : Text;
    tokenOutFee : Nat;
  };
  type Ledger = actor {
    icrc1_transfer : shared {
      from_subaccount : ?Blob; to : Account; amount : Nat; fee : ?Nat; memo : ?Blob; created_at_time : ?Nat64;
    } -> async { #Ok : Nat; #Err : Any };
    icrc2_transfer_from : shared {
      spender_subaccount : ?Blob; from : Account; to : Account; amount : Nat; fee : ?Nat; memo : ?Blob; created_at_time : ?Nat64;
    } -> async { #Ok : Nat; #Err : Any };
  };

  func parse(t : Text) : ?Nat {
    var n = 0;
    if (t.size() == 0) return null;
    for (c in t.chars()) {
      if (c < '0' or c > '9') return null;
      n := n * 10 + Nat32.toNat(Char.toNat32(c) - 48);
    };
    ?n;
  };

  func out(amountIn : Nat, zeroForOne : Bool) : Nat {
    if (zeroForOne) { amountIn * rate0to1 / rateDen } else { amountIn * rateDen / rate0to1 };
  };

  public query func metadata() : async { #ok : PoolMetadata; #err : Error } {
    #ok({
      fee = 3000;
      key = Principal.toText(token0) # "_" # Principal.toText(token1) # "_3000";
      sqrtPriceX96 = 0;
      tick = 0;
      liquidity = 0;
      token0 = { address = Principal.toText(token0); standard = "ICRC2" };
      token1 = { address = Principal.toText(token1); standard = "ICRC2" };
      maxLiquidityPerTick = 0;
      nextPositionId = 1;
    });
  };

  public query func quote(args : SwapArgs) : async { #ok : Nat; #err : Error } {
    switch (parse(args.amountIn)) {
      case (?amount) #ok(out(amount, args.zeroForOne));
      case null #err(#InternalError("amountIn"));
    };
  };

  public shared ({ caller }) func depositFromAndSwap(args : DepositAndSwapArgs) : async { #ok : Nat; #err : Error } {
    let ?amountIn = parse(args.amountIn) else return #err(#InternalError("amountIn"));
    let ?minimum = parse(args.amountOutMinimum) else return #err(#InternalError("amountOutMinimum"));
    let gross = out(amountIn, args.zeroForOne);
    if (gross < minimum + args.tokenOutFee) return #err(#InternalError("slippage"));
    let (tokenIn, tokenOut) = if (args.zeroForOne) (token0, token1) else (token1, token0);
    let ledgerIn : Ledger = actor (Principal.toText(tokenIn));
    let ledgerOut : Ledger = actor (Principal.toText(tokenOut));
    switch (await ledgerIn.icrc2_transfer_from({
      spender_subaccount = null;
      from = { owner = caller; subaccount = null };
      to = { owner = Principal.fromActor(this); subaccount = null };
      amount = amountIn;
      fee = null;
      memo = null;
      created_at_time = null;
    })) {
      case (#Err(_)) return #err(#InsufficientFunds);
      case (#Ok(_)) {};
    };
    let paid = gross - args.tokenOutFee;
    switch (await ledgerOut.icrc1_transfer({
      from_subaccount = null;
      to = { owner = caller; subaccount = null };
      amount = paid;
      fee = null;
      memo = null;
      created_at_time = null;
    })) {
      case (#Err(_)) #err(#InternalError("the pool could not pay"));
      case (#Ok(_)) #ok(paid);
    };
  };
};
