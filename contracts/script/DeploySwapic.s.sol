// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Script, console2} from "forge-std/Script.sol";
import {Swapic} from "../src/Swapic.sol";

interface ITransmitterDomain {
    function localDomain() external view returns (uint32);
}

/// Deploys Swapic on the current chain (Base, Arbitrum, Ethereum or Polygon);
/// its constructor deploys the CctpReceiver and the Runner, so this is the
/// whole deployment and nothing is configured afterwards:
///
///   forge script script/DeploySwapic.s.sol --rpc-url <chain> --broadcast
///       env: CANISTER (required: the canister's EVM address),
///            OWNER (default: the broadcaster; any other OWNER is proposed
///            and must call acceptOwnership), GUARDIAN (default: CANISTER),
///            CK_HELPER (Ethereum only; default the ck minter's helper),
///            DEPLOYER_MNEMONIC + DEPLOYER_ADDRESS (optional: sign with the
///            phrase's key instead of --private-key or --account)
///       prints SWAPIC=, SWAPIC_RUNNER=, SWAPIC_CCTP_RECEIVER=
contract DeploySwapic is Script {
    address constant TRANSMITTER = 0x81D40F21F12A8F0E3252Bccb954D722d4c464B64;
    address constant TOKEN_MESSENGER = 0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d;
    /// the ck minter's ERC-20 deposit helper with subaccounts
    address constant CK_HELPER = 0x18901044688D3756C35Ed2b36D93e6a5B8e00E68;

    function usdcOf(uint256 chainId) public pure returns (address) {
        if (chainId == 8453) return 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913;
        if (chainId == 42161) return 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
        if (chainId == 1) return 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48;
        if (chainId == 137) return 0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359;
        revert("unsupported chain");
    }

    /// WETH, and WPOL on Polygon
    function wrappedOf(uint256 chainId) public pure returns (address) {
        if (chainId == 8453) return 0x4200000000000000000000000000000000000006;
        if (chainId == 42161) return 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
        if (chainId == 1) return 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2;
        if (chainId == 137) return 0x0d500B1d8E8eF31E21C99d1Db9A6444d3ADf1270;
        revert("unsupported chain");
    }

    function domainOf(uint256 chainId) public pure returns (uint32) {
        if (chainId == 8453) return 6;
        if (chainId == 42161) return 3;
        if (chainId == 1) return 0;
        if (chainId == 137) return 7;
        revert("unsupported chain");
    }

    function run() external returns (Swapic swapic) {
        address usdc = usdcOf(block.chainid);
        require(usdc.code.length > 0, "no USDC here");
        address wrapped = wrappedOf(block.chainid);
        require(wrapped.code.length > 0, "no wrapped native here");
        require(TRANSMITTER.code.length > 0, "no MessageTransmitterV2 here");
        require(TOKEN_MESSENGER.code.length > 0, "no TokenMessengerV2 here");
        require(ITransmitterDomain(TRANSMITTER).localDomain() == domainOf(block.chainid), "wrong CCTP domain");
        address ckHelper = block.chainid == 1 ? vm.envOr("CK_HELPER", CK_HELPER) : address(0);
        require(ckHelper == address(0) || ckHelper.code.length > 0, "no ck helper here");
        address canister = vm.envAddress("CANISTER");
        address guardian = vm.envOr("GUARDIAN", canister);

        // With DEPLOYER_MNEMONIC in the environment the key is derived here, in the script, on
        // MetaMask's path, and must be DEPLOYER_ADDRESS; it never appears on a command line.
        string memory phrase = vm.envOr("DEPLOYER_MNEMONIC", string(""));
        if (bytes(phrase).length > 0) {
            uint256 key = vm.deriveKey(phrase, uint32(vm.envOr("DEPLOYER_EVM_INDEX", uint256(0))));
            require(vm.addr(key) == vm.envAddress("DEPLOYER_ADDRESS"), "the phrase does not give DEPLOYER_ADDRESS");
            vm.startBroadcast(key);
        } else {
            vm.startBroadcast();
        }
        (, address me,) = vm.readCallers();
        address owner = vm.envOr("OWNER", me);
        swapic = new Swapic(usdc, TRANSMITTER, TOKEN_MESSENGER, wrapped, ckHelper, me, guardian, canister);
        if (owner != me) swapic.transferOwnership(owner);
        vm.stopBroadcast();

        console2.log("SWAPIC_CHAIN_ID=%s", block.chainid);
        console2.log("SWAPIC=%s", address(swapic));
        console2.log("SWAPIC_RUNNER=%s", swapic.runner());
        console2.log("SWAPIC_CCTP_RECEIVER=%s", swapic.cctpReceiver());
        console2.log("SWAPIC_CANISTER=%s", canister);
        console2.log("SWAPIC_GUARDIAN=%s", guardian);
        console2.log("SWAPIC_OWNER=%s", me);
        if (owner != me) console2.log("SWAPIC_PENDING_OWNER=%s", owner);
        console2.log("SWAPIC_CK_HELPER=%s", ckHelper);
        console2.log("SWAPIC_WRAPPED_NATIVE=%s", wrapped);
    }
}
