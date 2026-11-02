// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import {DeploySwapic} from "../script/DeploySwapic.s.sol";
import {Swapic} from "../src/Swapic.sol";

/// The deploy script's per-chain table against each chain's official USDC, wrapped native and
/// Circle CCTP domain listings (checked 2026-10-24), for the four chains deployed first and the
/// six added on 2026-10-19. A chain not offered is refused: Arc (5042, later: USDC is its gas
/// token and it has no wrapped native), and any chain not listed.
contract DeploySwapicTest is Test {
    DeploySwapic script = new DeploySwapic();

    function _check(uint256 chainId, address usdc, address wrapped, uint32 domain) internal view {
        assertEq(script.usdcOf(chainId), usdc, "USDC");
        assertEq(script.wrappedOf(chainId), wrapped, "wrapped native");
        assertEq(script.domainOf(chainId), domain, "CCTP domain");
    }

    function test_the_first_four_chains() public view {
        _check(8453, 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913, 0x4200000000000000000000000000000000000006, 6);
        _check(42161, 0xaf88d065e77c8cC2239327C5EDb3A432268e5831, 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1, 3);
        _check(1, 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48, 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2, 0);
        _check(137, 0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359, 0x0d500B1d8E8eF31E21C99d1Db9A6444d3ADf1270, 7);
    }

    function test_the_chains_added_on_2026_10_19() public view {
        // OP Mainnet: USDC, WETH, domain 2
        _check(10, 0x0b2C639c533813f4Aa9D7837CAf62653d097Ff85, 0x4200000000000000000000000000000000000006, 2);
        // Unichain: USDC, WETH, domain 10
        _check(130, 0x078D782b760474a361dDA0AF3839290b0EF57AD6, 0x4200000000000000000000000000000000000006, 10);
        // Monad: USDC, WMON, domain 15
        _check(143, 0x754704Bc059F8C67012fEd69BC8A327a5aafb603, 0x3bd359C1119dA7Da1D913D1C4D2B7c461115433A, 15);
        // HyperEVM: USDC, WHYPE, domain 19
        _check(999, 0xb88339CB7199b77E23DB6E890353E22632Ba630f, 0x5555555555555555555555555555555555555555, 19);
        // World Chain: USDC (USDC only: no KyberSwap there), WETH, domain 14
        _check(480, 0x79A02482A880bCE3F13e09Da970dC34db4CD24d1, 0x4200000000000000000000000000000000000006, 14);
        // Plasma: USDC, WXPL, domain 33
        _check(9745, 0x2d661C89D812261039AF9764eceaAee884f5F67F, 0x6100E367285b01F48D07953803A2d8dCA5D19873, 33);
    }

    function test_a_chain_the_doc_leaves_open_is_refused() public {
        uint256[3] memory refused = [uint256(5042), 146, 57073];
        for (uint256 i; i < refused.length; ++i) {
            vm.expectRevert("unsupported chain");
            script.usdcOf(refused[i]);
            vm.expectRevert("unsupported chain");
            script.wrappedOf(refused[i]);
            vm.expectRevert("unsupported chain");
            script.domainOf(refused[i]);
        }
    }

    /// Every chain gets the same three addresses: Swapic at the deployer's nonce 0, the
    /// CctpReceiver and the Runner at Swapic's own nonces 1 and 2.
    function test_the_addresses_are_the_deployers_nonce_zero_ones() public {
        address deployer = address(0xD5);
        address swapic = vm.computeCreateAddress(deployer, 0);
        vm.setNonce(deployer, 0);
        vm.prank(deployer);
        Swapic s = new Swapic(address(0x5757), address(0), address(0), address(0), address(0), deployer, address(0), address(0xCA));
        assertEq(address(s), swapic);
        assertEq(s.cctpReceiver(), vm.computeCreateAddress(swapic, 1));
        assertEq(s.runner(), vm.computeCreateAddress(swapic, 2));
    }
}
