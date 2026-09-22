// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/extensions/ERC20Permit.sol";

/// Circle's FiatTokenV2 shape: EIP-2612 and EIP-3009 on one token, with the two
/// checks that decide the door's properties copied verbatim in intent:
///   - `to == msg.sender`, so an authorization cannot be relayed by anyone but
///     its payee, which is why this door needs no front-run defence at all;
///   - both time bounds strict, `block.timestamp > validAfter` and
///     `block.timestamp < validBefore`, so equality on either side reverts.
/// The fee is off by default and exists only to prove the vault reports the
/// measured balance delta rather than the requested amount.
contract AuthorizationToken is ERC20Permit {
    /// keccak256("ReceiveWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)")
    bytes32 public constant RECEIVE_WITH_AUTHORIZATION_TYPEHASH =
        0xd099cc98ef71107a616c4f0f941f04c322d8e254fe26b3c6668db87aae413de8;

    mapping(address => mapping(bytes32 => bool)) private _authorizationStates;
    uint256 public feeBps;

    event AuthorizationUsed(address indexed authorizer, bytes32 indexed nonce);

    constructor() ERC20("Authorization", "AUTH") ERC20Permit("Authorization") {
        _mint(msg.sender, 1e27);
    }

    function setFeeBps(uint256 bps) external {
        feeBps = bps;
    }

    function authorizationState(address authorizer, bytes32 nonce) external view returns (bool) {
        return _authorizationStates[authorizer][nonce];
    }

    function receiveWithAuthorization(
        address from,
        address to,
        uint256 value,
        uint256 validAfter,
        uint256 validBefore,
        bytes32 nonce,
        uint8 v,
        bytes32 r,
        bytes32 s
    ) external {
        require(to == msg.sender, "FiatTokenV2: caller must be the payee");
        require(block.timestamp > validAfter, "FiatTokenV2: authorization is not yet valid");
        require(block.timestamp < validBefore, "FiatTokenV2: authorization is expired");
        require(!_authorizationStates[from][nonce], "FiatTokenV2: authorization is used or canceled");

        bytes32 structHash =
            keccak256(abi.encode(RECEIVE_WITH_AUTHORIZATION_TYPEHASH, from, to, value, validAfter, validBefore, nonce));
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", _domainSeparatorV4(), structHash));
        address signer = ecrecover(digest, v, r, s);
        require(signer != address(0) && signer == from, "FiatTokenV2: invalid signature");

        _authorizationStates[from][nonce] = true;
        emit AuthorizationUsed(from, nonce);
        _transfer(from, to, value);
    }

    function _update(address from, address to, uint256 value) internal override {
        uint256 fee = from == address(0) ? 0 : (value * feeBps) / 10_000;
        super._update(from, to, value - fee);
        if (fee > 0) super._update(from, address(0xdead), fee);
    }
}
