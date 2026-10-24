// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// CCTP v2's TokenMessengerV2 in miniature: it pulls the burn from the caller
/// and records the call, with the two refusals the quoter relies on.
contract CctpMessengerMock {
    struct Burn {
        uint256 amount;
        uint32 domain;
        bytes32 mintRecipient;
        address burnToken;
        bytes32 destinationCaller;
        uint256 maxFee;
        uint32 minFinality;
        bytes hookData;
    }

    Burn public last;

    function depositForBurnWithHook(
        uint256 amount,
        uint32 domain,
        bytes32 mintRecipient,
        address burnToken,
        bytes32 destinationCaller,
        uint256 maxFee,
        uint32 minFinality,
        bytes calldata hookData
    ) external {
        require(hookData.length > 0, "Hook data is empty");
        require(maxFee < amount, "Max fee must be less than amount");
        IERC20(burnToken).transferFrom(msg.sender, address(0xdead), amount);
        last = Burn(amount, domain, mintRecipient, burnToken, destinationCaller, maxFee, minFinality, hookData);
    }

    function lastHookData() external view returns (bytes memory) {
        return last.hookData;
    }
}

/// A DEX router that pays from its own float, to the caller.
contract DexMock {
    function swap(address tokenIn, address tokenOut, uint256 amountIn, uint256 amountOut) external {
        IERC20(tokenIn).transferFrom(msg.sender, address(this), amountIn);
        IERC20(tokenOut).transfer(msg.sender, amountOut);
    }

    /// spends one unit less than it was given
    function swapShort(address tokenIn, address tokenOut, uint256 amountIn, uint256 amountOut) external {
        IERC20(tokenIn).transferFrom(msg.sender, address(this), amountIn - 1);
        IERC20(tokenOut).transfer(msg.sender, amountOut);
    }

    function swapToEth(address tokenIn, uint256 amountIn, uint256 amountOut) external {
        IERC20(tokenIn).transferFrom(msg.sender, address(this), amountIn);
        (bool ok,) = msg.sender.call{value: amountOut}("");
        require(ok, "eth");
    }

    receive() external payable {}
}

/// KyberSwap's InputScalingHelper in miniature for `DexMock.swap` calldata:
/// it rescales amountIn and amountOut by newAmount / amountIn.
contract ScalerMock {
    bool public immutable ok;

    constructor(bool ok_) {
        ok = ok_;
    }

    function getScaledInputData(bytes calldata data, uint256 newAmount) external view returns (bool, bytes memory) {
        (address tokenIn, address tokenOut, uint256 amountIn, uint256 amountOut) =
            abi.decode(data[4:], (address, address, uint256, uint256));
        return (ok, abi.encodeCall(DexMock.swap, (tokenIn, tokenOut, newAmount, amountOut * newAmount / amountIn)));
    }
}

/// A rail whose amount sits inside nested calldata, at an offset that is not
/// a word boundary of the outer data (like an Eco route).
contract NestedRailMock {
    uint256 public last;

    function pullNested(address token, bytes calldata inner) external {
        uint256 amount = uint256(bytes32(inner[36:68]));
        IERC20(token).transferFrom(msg.sender, address(this), amount);
        last = amount;
    }
}

interface IMessageHandler {
    function handleReceiveFinalizedMessage(uint32 sourceDomain, bytes32 sender, uint32 finality, bytes calldata body)
        external
        returns (bool);
}

/// CCTP v2's MessageTransmitterV2 in miniature: no attestation check, but the
/// destination-caller rule, the nonce, and, like the real one, a call to the
/// header's recipient: the TokenMessenger mints `amount - feeExecuted` to the
/// mint recipient (here from the mock's own float), any other recipient is a
/// generic message handler.
contract TransmitterMock {
    address internal constant TOKEN_MESSENGER = 0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d;
    IERC20 public immutable usdc;
    mapping(bytes32 => bool) public usedNonces;

    constructor(IERC20 usdc_) {
        usdc = usdc_;
    }

    function receiveMessage(bytes calldata message, bytes calldata) external returns (bool) {
        bytes32 caller = bytes32(message[108:140]);
        require(caller == bytes32(0) || caller == bytes32(uint256(uint160(msg.sender))), "Invalid caller for message");
        bytes32 nonce = bytes32(message[12:44]);
        require(!usedNonces[nonce], "Nonce already used");
        usedNonces[nonce] = true;
        address recipient = address(uint160(uint256(bytes32(message[76:108]))));
        bytes calldata body = message[148:];
        if (recipient != TOKEN_MESSENGER) {
            return IMessageHandler(recipient).handleReceiveFinalizedMessage(
                uint32(bytes4(message[4:8])), bytes32(message[44:76]), 2000, body
            );
        }
        address to = address(uint160(uint256(bytes32(body[36:68]))));
        uint256 amount = uint256(bytes32(body[68:100]));
        uint256 fee = uint256(bytes32(body[164:196]));
        usdc.transfer(to, amount - fee);
        return true;
    }
}

/// The ck minter's deposit helper in miniature: pulls `amount` from the caller.
contract CkHelperMock {
    bytes32 public lastPrincipal;
    uint256 public lastAmount;

    function depositErc20(address token, uint256 amount, bytes32 principal, bytes32) external {
        IERC20(token).transferFrom(msg.sender, address(this), amount);
        lastPrincipal = principal;
        lastAmount = amount;
    }
}

/// Answers what the Runner and the receiver read from Swapic.
contract SwapicStub {
    bool public paused;
    bool public locked;
    address public canister;
    address public runner;

    function lock(bool locked_) external {
        locked = locked_;
    }

    function set(bool paused_, address canister_, address runner_) external {
        paused = paused_;
        canister = canister_;
        runner = runner_;
    }
}

contract RejectsEth {
    receive() external payable {
        revert("no");
    }
}

function burnGas(uint256 n) pure returns (uint256 x) {
    assembly ("memory-safe") {
        for { let i := 0 } lt(i, n) { i := add(i, 1) } {
            mstore(0, x)
            mstore(32, i)
            x := keccak256(0, 64)
        }
    }
}

contract Burner {
    /// burns in a frame `depth` calls below this one
    function burn(uint256 n, uint256 depth) external view returns (uint256) {
        return depth == 0 ? burnGas(n) : this.burn(n, depth - 1);
    }
}

/// A router that spends a lot of gas before it swaps: in its own frame
/// (`depth` 0) or `depth` calls deeper, like an aggregator's executor.
contract HungryDex {
    Burner public immutable burner = new Burner();

    function swap(address tokenIn, address tokenOut, uint256 amountIn, uint256 amountOut, uint256 n, uint256 depth)
        external
    {
        if (depth > 0) burner.burn(n, depth - 1);
        else burnGas(n);
        IERC20(tokenIn).transferFrom(msg.sender, address(this), amountIn);
        IERC20(tokenOut).transfer(msg.sender, amountOut);
    }
}

/// Fails by burning all the gas it is given (the INVALID opcode).
contract BurnsAll {
    fallback() external {
        assembly {
            invalid()
        }
    }
}

/// An attacker's generic message handler: when its message is received, it
/// relays someone's caller-zero burn that mints to the CctpReceiver.
contract RelayHandler {
    TransmitterMock public immutable transmitter;
    bytes public relay;

    constructor(TransmitterMock transmitter_) {
        transmitter = transmitter_;
    }

    function arm(bytes calldata relay_) external {
        relay = relay_;
    }

    function handleReceiveFinalizedMessage(uint32, bytes32, uint32, bytes calldata) external returns (bool) {
        return transmitter.receiveMessage(relay, "");
    }
}

/// A ck deposit helper that spends a lot of gas before it pulls.
contract HungryCkHelper {
    uint256 public lastAmount;

    function depositErc20(address token, uint256 amount, bytes32, bytes32) external {
        burnGas(30_000);
        IERC20(token).transferFrom(msg.sender, address(this), amount);
        lastAmount = amount;
    }
}

/// Like Eco's Portal: pulls the amount, then pushes any ether it holds back to
/// the caller.
contract ExcessNativeRail {
    function take(address token, uint256 amount) external {
        IERC20(token).transferFrom(msg.sender, address(this), amount);
        uint256 excess = address(this).balance;
        if (excess > 0) {
            (bool ok,) = msg.sender.call{value: excess}("");
            require(ok, "excess");
        }
    }

    receive() external payable {}
}
