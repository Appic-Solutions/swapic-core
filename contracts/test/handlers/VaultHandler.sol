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
/// plain calls while public-path calls are pranked as users. Every entry point
/// bounds its inputs so that a revert out of the handler is a handler bug, not
/// a wasted run: `fail_on_revert = true` turns that into a loud failure.
contract VaultHandler is Test {
    uint256 internal constant TOKEN_COUNT = 3;
    uint256 internal constant MAX_AMOUNT = 1000e18;
    uint256 internal constant ROUTER_FLOAT = 1e26;
    uint256 internal constant VAULT_SEED = 1e24;
    bytes32 internal constant ITEM_RESULT = keccak256("ItemResult(bytes32,bool)");

    struct Leg {
        address tokenIn;
        address tokenOut;
        uint256 amountIn;
        uint256 amountOut;
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
    uint256 public atomicSwaps;
    uint256 public retentions;
    uint256 public overApprovesAccepted;
    uint256 public overApprovesRejected;

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

    function canister_execute(
        uint256 inSeed,
        uint256 outSeed,
        uint256 amountInSeed,
        uint256 amountOutSeed,
        uint256 slackSeed
    ) external {
        (TestToken tokenIn, TestToken tokenOut) = _pair(inSeed, outSeed);
        uint256 pooled = tokenIn.balanceOf(address(vault));
        if (pooled == 0) return;

        uint256 amountIn = bound(amountInSeed, 1, pooled > MAX_AMOUNT ? MAX_AMOUNT : pooled);
        uint256 amountOut = bound(amountOutSeed, 0, MAX_AMOUNT);
        // approving more than the router pulls is the realistic shape: the
        // leftover allowance is what the reset in _runCalls has to clear
        uint256 approveAmount = amountIn + bound(slackSeed, 0, 1e18);

        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(router),
            0,
            abi.encodeCall(SwapRouterMock.swapAtoB, (address(tokenIn), address(tokenOut), amountIn, amountOut)),
            address(tokenIn),
            approveAmount
        );
        Vault.Delta[] memory deltas = new Vault.Delta[](2);
        deltas[0] = Vault.Delta(address(tokenIn), -int256(amountIn));
        deltas[1] = Vault.Delta(address(tokenOut), int256(amountOut));

        try vault.execute(_fresh("swap"), calls, deltas) {
            ghostOut[address(tokenIn)] += amountIn;
            ghostIn[address(tokenOut)] += amountOut;
            executes++;
        } catch {}
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
        uint256 amountIn = _batchAmountIn(tokenIn, s >> 16);
        uint256 amountOut = bound(s >> 32, 0, MAX_AMOUNT);

        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(router),
            0,
            abi.encodeCall(SwapRouterMock.swapAtoB, (address(tokenIn), address(tokenOut), amountIn, amountOut)),
            address(tokenIn),
            amountIn + bound(s >> 64, 0, 1e18)
        );
        Vault.Delta[] memory deltas = new Vault.Delta[](2);
        deltas[0] = Vault.Delta(address(tokenIn), -int256(amountIn));
        deltas[1] = Vault.Delta(address(tokenOut), int256(amountOut));

        return (
            Vault.Item(_fresh("item"), calls, deltas, gasLimit),
            Leg(address(tokenIn), address(tokenOut), amountIn, amountOut)
        );
    }

    function _evilItem(uint256 s, uint256 gasLimit) internal returns (Vault.Item memory, Leg memory) {
        (TestToken tokenIn, TestToken tokenOut) = _pair(s, s >> 8);
        uint256 amountIn = _batchAmountIn(tokenIn, s >> 16);

        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(evilRouter),
            0,
            abi.encodeCall(EvilRouterMock.swapAtoB, (address(tokenIn), address(tokenOut), amountIn, 0)),
            address(tokenIn),
            amountIn + bound(s >> 64, 0, 1e18)
        );
        Vault.Delta[] memory deltas = new Vault.Delta[](1);
        deltas[0] = Vault.Delta(address(tokenOut), int256(1)); // never paid: always missed

        return
            (Vault.Item(_fresh("item"), calls, deltas, gasLimit), Leg(address(tokenIn), address(tokenOut), amountIn, 0));
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
            } catch {}
        } else {
            try vault.payout(ref, address(t), to, amount) {
                ghostOut[address(t)] += amount;
                payouts++;
            } catch {}
        }
    }

    function user_atomic_swap(
        uint256 inSeed,
        uint256 outSeed,
        uint256 amountSeed,
        uint256 pullSeed,
        uint256 amountOutSeed,
        uint256 userSeed,
        bool keepInVault
    ) external {
        (TestToken tokenIn, TestToken tokenOut) = _pair(inSeed, outSeed);
        uint256 amount = bound(amountSeed, 1, MAX_AMOUNT);
        uint256 pull = bound(pullSeed, 0, amount);
        uint256 amountOut = bound(amountOutSeed, 0, MAX_AMOUNT);
        address user = _user(userSeed);
        address payoutTo = keepInVault ? address(0) : user;

        // approves the whole deposit but lets the router pull less: the residue
        // exercises the approval reset on the public path too
        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(router),
            0,
            abi.encodeCall(SwapRouterMock.swapAtoB, (address(tokenIn), address(tokenOut), pull, amountOut)),
            address(tokenIn),
            amount
        );

        _fund(tokenIn, user, amount);
        try vault.depositAndExecute(
            _fresh("atomic"), address(tokenIn), amount, calls, address(tokenOut), amountOut, payoutTo
        ) {
            ghostIn[address(tokenIn)] += amount;
            ghostOut[address(tokenIn)] += pull;
            ghostIn[address(tokenOut)] += amountOut;
            if (payoutTo != address(0) && amountOut > 0) ghostOut[address(tokenOut)] += amountOut;
            atomicSwaps++;
        } catch {}
        vm.stopPrank();
    }

    /// token == payoutToken with no calls: the deposit itself must never count
    /// toward minOut, so nothing may be paid straight back out
    function user_atomic_retention(uint256 tokenSeed, uint256 amountSeed, uint256 userSeed) external {
        TestToken t = _token(tokenSeed);
        uint256 amount = bound(amountSeed, 1, MAX_AMOUNT);
        address user = _user(userSeed);

        _fund(t, user, amount);
        vault.depositAndExecute(_fresh("retain"), address(t), amount, new Vault.Call[](0), address(t), 0, user);
        vm.stopPrank();

        ghostIn[address(t)] += amount;
        retentions++;
    }

    /// a stranger depositing dust while approving the vault's pooled balance:
    /// this must always revert, so the success arm credits nothing on purpose
    function user_atomic_over_approve(
        uint256 inSeed,
        uint256 outSeed,
        uint256 dustSeed,
        uint256 grabSeed,
        uint256 userSeed
    ) external {
        (TestToken tokenIn, TestToken tokenOut) = _pair(inSeed, outSeed);
        uint256 pooled = tokenIn.balanceOf(address(vault));
        if (pooled == 0) return;

        uint256 dust = bound(dustSeed, 1, 1e18);
        uint256 grab = dust + bound(grabSeed, 1, pooled);
        address user = _user(userSeed);

        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(router),
            0,
            abi.encodeCall(SwapRouterMock.swapAtoB, (address(tokenIn), address(tokenOut), grab, 0)),
            address(tokenIn),
            grab
        );

        _fund(tokenIn, user, dust);
        try vault.depositAndExecute(_fresh("grab"), address(tokenIn), dust, calls, address(tokenOut), 0, user) {
            overApprovesAccepted++;
        } catch {
            overApprovesRejected++;
        }
        vm.stopPrank();
    }
}
