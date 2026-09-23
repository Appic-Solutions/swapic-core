// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../../src/Vault.sol";
import "../mocks/TestToken.sol";
import "../mocks/SwapRouterMock.sol";
import "../mocks/EvilRouterMock.sol";

/// The invariant fuzzer's only entry point. It owns the whole world (vault,
/// tokens, routers) and doubles as the canister, so canister-only calls are
/// plain calls while a user's calls are pranked as that user. Every entry point
/// bounds its inputs so that a revert out of the handler is a handler bug, not
/// a wasted run: `fail_on_revert = true` turns that into a loud failure.
contract VaultHandler is Test {
    uint256 internal constant TOKEN_COUNT = 3;
    uint256 internal constant MAX_AMOUNT = 1000e18;
    uint256 internal constant ROUTER_FLOAT = 1e26;
    uint256 internal constant VAULT_SEED = 1e24;
    bytes32 internal constant ITEM_RESULT = keccak256("ItemResult(bytes32,bool)");
    /// the removed public door's exact signature, called the only way it still can be
    bytes4 internal constant DEPOSIT_AND_EXECUTE = bytes4(
        keccak256(
            "depositAndExecute(bytes32,address,uint256,(address,uint256,bytes,address,uint256)[],address,uint256,address)"
        )
    );

    struct Leg {
        address tokenIn;
        address tokenOut;
        uint256 amountIn;
        uint256 amountOut;
        bool evil;
    }

    Vault public immutable vault;
    SwapRouterMock public immutable router;
    EvilRouterMock public immutable evilRouter;
    TestToken[TOKEN_COUNT] public tokens;

    /// everything that should have entered / left the vault, per token
    mapping(address => uint256) public ghostIn;
    mapping(address => uint256) public ghostOut;

    uint256 public deposits;
    uint256 public executes;
    uint256 public batchItems;
    uint256 public payouts;
    /// a stranger's reach for a target, per door: accepted must stay zero, and a
    /// refusal for any reason but the door's own is a misfire
    uint256 public strangerCallsAccepted;
    uint256 public strangerCallsRefused;
    uint256 public strangerCallsMisfired;

    /// catch arms that must never fire: these entry points swallow reverts so a
    /// bad bound cannot abort a run, which would otherwise hide a starved handler
    uint256 public executeFailures;
    uint256 public payoutFailures;

    uint256 private _nonce;

    constructor(address guardian_) {
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (address(this), guardian_));
        Vault v = Vault(payable(address(new ERC1967Proxy(address(impl), init))));
        SwapRouterMock r = new SwapRouterMock();
        EvilRouterMock e = new EvilRouterMock();
        vault = v;
        router = r;
        evilRouter = e;

        for (uint256 i = 0; i < TOKEN_COUNT; i++) {
            TestToken t = new TestToken();
            tokens[i] = t;
            t.transfer(address(r), ROUTER_FLOAT);
            t.transfer(address(v), VAULT_SEED);
            ghostIn[address(t)] += VAULT_SEED;
        }

        v.setRouterAllowlist(address(r), true);
        v.setRouterAllowlist(address(e), true);
    }

    /// fresh by construction: a pair the vault has already burned would revert
    /// and quietly starve the run of deposits
    function _fresh(string memory tag) internal returns (bytes32) {
        return keccak256(abi.encode(tag, _nonce++));
    }

    function _token(uint256 seed) internal view returns (TestToken) {
        return tokens[bound(seed, 0, TOKEN_COUNT - 1)];
    }

    function _pair(uint256 inSeed, uint256 outSeed) internal view returns (TestToken tokenIn, TestToken tokenOut) {
        uint256 i = bound(inSeed, 0, TOKEN_COUNT - 1);
        uint256 step = bound(outSeed, 1, TOKEN_COUNT - 1);
        tokenIn = tokens[i];
        tokenOut = tokens[(i + step) % TOKEN_COUNT];
    }

    /// a small pool of plain EOAs: never a token, the vault or a router
    function _user(uint256 seed) internal pure returns (address) {
        return address(uint160(0xB0B00000 + (seed % 8)));
    }

    function _fund(TestToken t, address user, uint256 amount) internal {
        t.transfer(user, amount);
        vm.startPrank(user);
        t.approve(address(vault), amount);
    }

    function user_deposit(uint256 tokenSeed, uint256 amountSeed, uint256 userSeed) external {
        TestToken t = _token(tokenSeed);
        uint256 amount = bound(amountSeed, 1, MAX_AMOUNT);
        _fund(t, _user(userSeed), amount);
        vault.deposit(_fresh("deposit"), address(t), amount);
        vm.stopPrank();

        ghostIn[address(t)] += amount;
        deposits++;
    }

    /// splits one leg across 1-3 router calls, so a bug that only bites at call
    /// index > 0 (a missed approve, a missed reset, a skipped target check) is
    /// reachable. The parts sum to the leg, so the deltas stay exact.
    /// Both mocks share the swapAtoB(address,address,uint256,uint256) signature,
    /// so one encoding reaches either: if that ever diverges, evil items would
    /// fail on a missing selector instead of on the delta check, and the batch
    /// delta check would go unfalsifiable again.
    function _splitCalls(Leg memory leg, uint256 n, uint256 slack) internal view returns (Vault.Call[] memory calls) {
        calls = new Vault.Call[](n);
        uint256 inLeft = leg.amountIn;
        uint256 outLeft = leg.amountOut;
        for (uint256 k = 0; k < n; k++) {
            uint256 inPart = k + 1 == n ? inLeft : inLeft / (n - k);
            uint256 outPart = k + 1 == n ? outLeft : outLeft / (n - k);
            inLeft -= inPart;
            outLeft -= outPart;
            calls[k] = Vault.Call(
                leg.evil ? address(evilRouter) : address(router),
                0,
                abi.encodeCall(SwapRouterMock.swapAtoB, (leg.tokenIn, leg.tokenOut, inPart, outPart)),
                leg.tokenIn,
                // approving more than the router pulls is the realistic shape:
                // the leftover allowance is what the reset in _runCalls clears
                inPart + slack
            );
        }
    }

    function canister_execute(
        uint256 inSeed,
        uint256 outSeed,
        uint256 amountInSeed,
        uint256 amountOutSeed,
        uint256 slackSeed,
        uint256 callsSeed
    ) external {
        (TestToken tokenIn, TestToken tokenOut) = _pair(inSeed, outSeed);
        uint256 pooled = tokenIn.balanceOf(address(vault));
        if (pooled == 0) return;

        Leg memory leg = Leg(
            address(tokenIn),
            address(tokenOut),
            bound(amountInSeed, 1, pooled > MAX_AMOUNT ? MAX_AMOUNT : pooled),
            bound(amountOutSeed, 0, MAX_AMOUNT),
            false
        );
        Vault.Delta[] memory deltas = new Vault.Delta[](2);
        deltas[0] = Vault.Delta(leg.tokenIn, -int256(leg.amountIn));
        deltas[1] = Vault.Delta(leg.tokenOut, int256(leg.amountOut));

        try vault.execute(_fresh("swap"), _splitCalls(leg, bound(callsSeed, 1, 3), bound(slackSeed, 0, 1e18)), deltas) {
            ghostOut[leg.tokenIn] += leg.amountIn;
            ghostIn[leg.tokenOut] += leg.amountOut;
            executes++;
        } catch {
            executeFailures++;
        }
    }

    function canister_execute_many(uint256 countSeed, uint256 seed, uint256 gasSeed) external {
        uint256 n = bound(countSeed, 1, 3);
        Vault.Item[] memory items = new Vault.Item[](n);
        Leg[] memory legs = new Leg[](n);

        for (uint256 k = 0; k < n; k++) {
            uint256 s = uint256(keccak256(abi.encode(seed, k)));
            uint256 gasLimit = (gasSeed >> k) % 4 == 0 ? 400_000 : 0;
            if (s % 3 == 0) (items[k], legs[k]) = _evilItem(s, gasLimit);
            else (items[k], legs[k]) = _goodItem(s, gasLimit);
        }
        _runBatch(items, legs);
    }

    /// a third each: items in one batch compete for the same vault balance
    function _batchAmountIn(TestToken tokenIn, uint256 seed) internal view returns (uint256) {
        uint256 room = tokenIn.balanceOf(address(vault)) / TOKEN_COUNT;
        return bound(seed, 0, room > MAX_AMOUNT ? MAX_AMOUNT : room);
    }

    function _goodItem(uint256 s, uint256 gasLimit) internal returns (Vault.Item memory, Leg memory) {
        (TestToken tokenIn, TestToken tokenOut) = _pair(s, s >> 8);
        Leg memory leg = Leg(
            address(tokenIn), address(tokenOut), _batchAmountIn(tokenIn, s >> 16), bound(s >> 32, 0, MAX_AMOUNT), false
        );
        Vault.Delta[] memory deltas = new Vault.Delta[](2);
        deltas[0] = Vault.Delta(leg.tokenIn, -int256(leg.amountIn));
        deltas[1] = Vault.Delta(leg.tokenOut, int256(leg.amountOut));

        Vault.Call[] memory calls = _splitCalls(leg, bound(s >> 96, 1, 3), bound(s >> 64, 0, 1e18));
        return (Vault.Item(_fresh("item"), calls, deltas, gasLimit), leg);
    }

    function _evilItem(uint256 s, uint256 gasLimit) internal returns (Vault.Item memory, Leg memory) {
        (TestToken tokenIn, TestToken tokenOut) = _pair(s, s >> 8);
        Leg memory leg = Leg(address(tokenIn), address(tokenOut), _batchAmountIn(tokenIn, s >> 16), 0, true);

        Vault.Delta[] memory deltas = new Vault.Delta[](1);
        deltas[0] = Vault.Delta(leg.tokenOut, int256(1)); // never paid: always missed

        Vault.Call[] memory calls = _splitCalls(leg, 1, bound(s >> 64, 0, 1e18));
        return (Vault.Item(_fresh("item"), calls, deltas, gasLimit), leg);
    }

    /// the batch swallows per-item failures, so the vault's own ItemResult log is
    /// the only honest record of which legs actually landed
    function _runBatch(Vault.Item[] memory items, Leg[] memory legs) internal {
        vm.recordLogs();
        vault.executeMany(items);
        Vm.Log[] memory logs = vm.getRecordedLogs();

        uint256 seen;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].emitter != address(vault) || logs[i].topics.length == 0) continue;
            if (logs[i].topics[0] != ITEM_RESULT) continue;
            if (abi.decode(logs[i].data, (bool))) {
                // the evil router never pays, so its delta can only be missed and
                // a gas-capped item can only report false: a success here means
                // the delta check stopped biting
                assertFalse(legs[seen].evil, "evil batch item reported success");
                ghostOut[legs[seen].tokenIn] += legs[seen].amountIn;
                ghostIn[legs[seen].tokenOut] += legs[seen].amountOut;
                batchItems++;
            }
            seen++;
        }
    }

    function canister_payout(uint256 tokenSeed, uint256 amountSeed, uint256 userSeed, bool asRefund) external {
        TestToken t = _token(tokenSeed);
        uint256 credited = ghostIn[address(t)];
        uint256 spent = ghostOut[address(t)];
        if (spent >= credited) return;

        uint256 amount = bound(amountSeed, 1, credited - spent);
        address to = _user(userSeed);
        bytes32 ref = _fresh("payout");

        if (asRefund) {
            try vault.refund(ref, address(t), to, amount) {
                ghostOut[address(t)] += amount;
                payouts++;
            } catch {
                payoutFailures++;
            }
        } else {
            try vault.payout(ref, address(t), to, amount) {
                ghostOut[address(t)] += amount;
                payouts++;
            } catch {
                payoutFailures++;
            }
        }
    }

    /// A stranger reaching for the pool through every door that makes the vault
    /// issue a call, and through the removed public door's selector with the
    /// honest swap it used to accept. None may ever land, and each must refuse
    /// for its own reason, so the success arm credits nothing on purpose.
    function stranger_reaches_for_a_target(
        uint256 inSeed,
        uint256 outSeed,
        uint256 amountSeed,
        uint256 amountOutSeed,
        uint256 userSeed
    ) external {
        (TestToken tokenIn, TestToken tokenOut) = _pair(inSeed, outSeed);
        uint256 pooled = tokenIn.balanceOf(address(vault));
        if (pooled == 0) return;
        address stranger = _user(userSeed);

        // the evil router takes what it is approved and pays nothing back
        Vault.Call[] memory drain = _splitCalls(
            Leg(
                address(tokenIn),
                address(tokenOut),
                bound(amountSeed, 1, pooled > MAX_AMOUNT ? MAX_AMOUNT : pooled),
                0,
                true
            ),
            1,
            0
        );
        // what the removed door accepted: the stranger's own deposit, spent in
        // full through a listed router, proceeds paid to the stranger
        uint256 deposit = bound(amountSeed, 1, MAX_AMOUNT);
        Vault.Call[] memory honest = _splitCalls(
            Leg(address(tokenIn), address(tokenOut), deposit, bound(amountOutSeed, 0, MAX_AMOUNT), false), 1, 0
        );
        bytes memory publicDoor = abi.encodeWithSelector(
            DEPOSIT_AND_EXECUTE,
            _fresh("public"),
            address(tokenIn),
            deposit,
            honest,
            address(tokenOut),
            uint256(0),
            stranger
        );

        _fund(tokenIn, stranger, deposit); // leaves the prank running as the stranger
        _attemptEveryDoor(drain, publicDoor);
        vm.stopPrank();
    }

    function _attemptEveryDoor(Vault.Call[] memory calls, bytes memory publicDoor) internal {
        Vault.Delta[] memory none = new Vault.Delta[](0);
        Vault.Item[] memory items = new Vault.Item[](1);
        items[0] = Vault.Item(_fresh("stranger"), calls, none, 0);
        bytes[] memory selfCalls = new bytes[](1);
        selfCalls[0] = abi.encodeCall(Vault.execute, (items[0].swapRef, calls, none));
        bytes memory onlyCanister = abi.encodeWithSelector(Vault.OnlyCanister.selector);

        _attempt(abi.encodeCall(Vault.execute, (items[0].swapRef, calls, none)), onlyCanister);
        _attempt(abi.encodeCall(Vault.executeMany, (items)), onlyCanister);
        _attempt(abi.encodeCall(Vault.multicall, (selfCalls)), onlyCanister);
        _attempt(abi.encodeCall(Vault.runItem, (items[0])), abi.encodeWithSelector(Vault.OnlySelf.selector));
        _attempt(publicDoor, "");
    }

    function _attempt(bytes memory data, bytes memory reason) internal {
        (bool ok, bytes memory ret) = address(vault).call(data);
        if (ok) strangerCallsAccepted++;
        else if (keccak256(ret) == keccak256(reason)) strangerCallsRefused++;
        else strangerCallsMisfired++;
    }
}
