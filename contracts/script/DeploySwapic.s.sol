// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Script, console2} from "forge-std/Script.sol";
import {SwapicRouter} from "../src/SwapicRouter.sol";
import {SwapicExecutor} from "../src/SwapicExecutor.sol";

/// Deploys a SwapicRouter and a SwapicExecutor on the current chain (Base,
/// Arbitrum or Ethereum, mainnet or an anvil fork of it), then links remote
/// chains. Two steps, because each chain's registry needs the other chain's
/// executor:
///
///   forge script script/DeploySwapic.s.sol --rpc-url <fork> --broadcast
///       env: KEEPER (default: the broadcaster), RELAYER (default: none),
///            GUARDIAN (default: the broadcaster)
///       prints SWAPIC_ROUTER=0x... and SWAPIC_EXECUTOR=0x...
///   forge script script/DeploySwapic.s.sol --sig "link(address,uint64,address)" \
///       <local router> <remote chain id> <remote executor> --rpc-url <fork> --broadcast
///
/// The allowlists get only the stateless DEX routers that have code here:
/// KyberSwap's MetaAggregationRouterV2, 0x's AllowanceHolder and Velora's
/// AugustusV6.2. Never a token, never a rail.
contract DeploySwapic is Script {
    address constant TOKEN_MESSENGER = 0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d;
    address constant TRANSMITTER = 0x81D40F21F12A8F0E3252Bccb954D722d4c464B64;
    address constant ECO_PORTAL = 0xEC000064576f9C95a8623Bc0eff3db6d296ea6df;
    address constant HYPER_PROVER = 0xec004Ab4870c4e177c66949329dCdb503CE41022;
    /// the ck minter's ERC-20 deposit helper with subaccounts (Ethereum only;
    /// `depositErc20` selector 0xdb9751af present in its code, checked with cast)
    address constant CK_HELPER = 0x18901044688D3756C35Ed2b36D93e6a5B8e00E68;

    address constant KYBER = 0x6131B5fae19EA4f9D964eAc0408E4408b66337b5;
    address constant ZEROX_ALLOWANCE_HOLDER = 0x0000000000001fF3684f28c67538d4D072C22734;
    address constant VELORA = 0x6A000F20005980200259B80c5102003040001068;

    function usdcOf(uint256 chainId) public pure returns (address) {
        if (chainId == 8453) return 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913;
        if (chainId == 42161) return 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
        if (chainId == 1) return 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48;
        revert("unsupported chain");
    }

    function domainOf(uint256 chainId) public pure returns (uint32) {
        if (chainId == 8453) return 6;
        if (chainId == 42161) return 3;
        if (chainId == 1) return 0;
        revert("unsupported chain");
    }

    function run() external returns (SwapicRouter router, SwapicExecutor executor) {
        address usdc = usdcOf(block.chainid);
        vm.startBroadcast();
        (, address me,) = vm.readCallers();
        address keeper = vm.envOr("KEEPER", me);
        address relayer = vm.envOr("RELAYER", address(0));
        address guardian = vm.envOr("GUARDIAN", me);

        router = new SwapicRouter(usdc, TOKEN_MESSENGER, ECO_PORTAL, me, guardian, relayer);
        executor = new SwapicExecutor(usdc, TRANSMITTER, block.chainid == 1 ? CK_HELPER : address(0), me, guardian);
        router.setEcoConfig(HYPER_PROVER, 2 hours);
        executor.setKeeper(keeper, true);

        address[3] memory dexes = [KYBER, ZEROX_ALLOWANCE_HOLDER, VELORA];
        for (uint256 i = 0; i < dexes.length; i++) {
            if (dexes[i].code.length == 0) continue;
            router.setAllowedTarget(dexes[i], true);
            executor.setAllowedTarget(dexes[i], true);
        }
        vm.stopBroadcast();

        console2.log("SWAPIC_CHAIN_ID=%s", block.chainid);
        console2.log("SWAPIC_ROUTER=%s", address(router));
        console2.log("SWAPIC_EXECUTOR=%s", address(executor));
    }

    /// Points the local router at a remote chain's executor, for both rails.
    function link(address router, uint64 remoteChainId, address remoteExecutor) external {
        vm.startBroadcast();
        SwapicRouter(router).setCctpDestination(
            remoteChainId, domainOf(remoteChainId), bytes32(uint256(uint160(remoteExecutor))), true
        );
        SwapicRouter(router).setEcoDestination(remoteChainId, ECO_PORTAL, usdcOf(remoteChainId), remoteExecutor);
        vm.stopBroadcast();
    }
}
