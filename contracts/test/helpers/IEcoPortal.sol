// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// The slice of Eco's Portal 2.12 (`0xEC000769A73b70e16f361a442292500b3BCf4A85`,
/// `version()` "2.12.0" on Base and Arbitrum) the fork tests use, with Eco's
/// struct layouts (ABI from Sourcify). The Portal hashes the route bytes it
/// is given; the destination hashes `abi.encode(route)`.
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

    event IntentPublished(
        bytes32 indexed intentHash,
        uint64 destination,
        bytes route,
        address indexed creator,
        address indexed prover,
        uint64 rewardDeadline,
        uint256 rewardNativeAmount,
        TokenAmount[] rewardTokens
    );

    /// selector 0xdf00f8fa
    function publishAndFund(uint64 destination, bytes calldata route, Reward calldata reward, bool allowPartial)
        external
        payable
        returns (bytes32 intentHash, address vault);

    function getIntentHash(uint64 destination, bytes calldata route, Reward calldata reward)
        external
        pure
        returns (bytes32 intentHash, bytes32 routeHash, bytes32 rewardHash);

    function intentVaultAddress(uint64 destination, bytes calldata route, Reward calldata reward)
        external
        view
        returns (address);

    function isIntentFunded(uint64 destination, bytes calldata route, Reward calldata reward)
        external
        view
        returns (bool);

    function version() external pure returns (string memory);
}
