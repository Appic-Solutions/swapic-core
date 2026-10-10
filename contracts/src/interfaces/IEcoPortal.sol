// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// The slice of Eco's Portal the router uses (live: `0xEC000769A73b70e16f361a442292500b3BCf4A85`,
/// version "2.12.0" on Base and Arbitrum; the older "2.6" Portal `0xEC000064...` has
/// the same surface), with Eco's own struct layouts from
/// `contracts/types/Intent.sol`. The Portal hashes the route bytes it is given,
/// and the destination hashes `abi.encode(route)`, so the router only ever
/// publishes the canonical encoding it built itself.
interface IEcoPortal {
    struct TokenAmount {
        address token;
        uint256 amount;
    }

    struct Call {
        address target;
        bytes data;
        uint256 value;
    }

    struct Route {
        bytes32 salt;
        uint64 deadline;
        address portal;
        uint256 nativeAmount;
        TokenAmount[] tokens;
        Call[] calls;
    }

    struct Reward {
        uint64 deadline;
        address creator;
        address prover;
        uint256 nativeAmount;
        TokenAmount[] tokens;
    }

    /// selector 0xdf00f8fa
    function publishAndFund(uint64 destination, bytes calldata route, Reward calldata reward, bool allowPartial)
        external
        payable
        returns (bytes32 intentHash, address vault);

    /// destination side, selector 0x52a8339e (used by the stand-in solver in tests)
    function fulfill(bytes32 intentHash, Route calldata route, bytes32 rewardHash, bytes32 claimant)
        external
        payable
        returns (bytes[] memory);
}
