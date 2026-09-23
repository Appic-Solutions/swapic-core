// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// Eco's Portal in miniature, the worked example of a listed target that holds
/// balances the vault can claim. It keeps the three exits that decide whether
/// listing it is safe, with Eco's authorization rules:
///   - `publishAndFund` pulls the reward from the caller and holds it past the
///     transaction (Eco holds it in a per-intent escrow clone; here the mock
///     holds it itself, which is the same thing to the vault);
///   - `refund` is permissionless after the deadline and pays `reward.creator`;
///   - `refundTo` is allowed only when `msg.sender == reward.creator`, and pays
///     whoever the caller names.
/// With the vault as creator, the vault itself is the only key to `refundTo`,
/// which is exactly why only the canister may ever make the vault call this
/// contract, and why the vault has no public door that runs calls.
contract EcoPortalMock {
    struct Reward {
        uint64 deadline;
        address creator;
        address token;
        uint256 amount;
    }

    mapping(bytes32 => bool) public escrowed;

    function intentHash(bytes32 routeHash, Reward calldata reward) public pure returns (bytes32) {
        return keccak256(abi.encode(routeHash, reward));
    }

    function publishAndFund(bytes32 routeHash, Reward calldata reward) external returns (bytes32 hash) {
        hash = intentHash(routeHash, reward);
        require(!escrowed[hash], "Portal: intent already funded");
        escrowed[hash] = true;
        require(IERC20(reward.token).transferFrom(msg.sender, address(this), reward.amount), "Portal: funding failed");
    }

    function refund(bytes32 routeHash, Reward calldata reward) external {
        _release(routeHash, reward, reward.creator);
    }

    function refundTo(bytes32 routeHash, Reward calldata reward, address refundee) external {
        require(msg.sender == reward.creator, "Portal: not the creator");
        _release(routeHash, reward, refundee);
    }

    function _release(bytes32 routeHash, Reward calldata reward, address to) internal {
        require(block.timestamp > reward.deadline, "Portal: reward still live");
        bytes32 hash = intentHash(routeHash, reward);
        require(escrowed[hash], "Portal: nothing escrowed");
        escrowed[hash] = false;
        require(IERC20(reward.token).transfer(to, reward.amount), "Portal: release failed");
    }
}
