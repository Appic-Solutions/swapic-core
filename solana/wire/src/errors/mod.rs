//! The program's custom error codes (design 4.15). One list, so the canister, the watcher and
//! the quoter read a failed transaction by the same numbers the program returns.

/// `ProgramError::Custom(code)` on chain.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum SwapicError {
    WrongConfig = 100,
    WrongAuth = 101,
    WrongVault = 102,
    WrongRent = 103,
    WrongEvent = 104,
    WrongFeeAccount = 105,
    WrongKey = 106,
    WrongProgram = 107,
    BadOutputAccount = 108,
    ForbiddenRouteAccount = 109,
    NotTopLevel = 110,

    ShortData = 200,
    ReservedFlags = 201,
    BadAction = 202,
    DomainNotAllowed = 203,
    DeadlineOutOfWindow = 204,
    BadRecipient = 205,
    BadTokenOutOrMinOut = 206,
    BadPrincipal = 207,
    BadMinGas = 208,
    MemoWithoutDeliver = 209,
    BadMemoLength = 210,

    BadMessageLength = 300,
    BadHeader = 301,
    NotForThisAuthOrVault = 302,
    BadHook = 303,

    NothingProduced = 400,
    BelowMinUsdc = 401,
    FeeAboveProduced = 402,
    BelowMinBurn = 403,
    MaxFeeTooHigh = 404,
    VaultNotAtBaseline = 405,
    BelowMinOut = 406,
    NothingMinted = 407,

    Paused = 500,
    DeadlinePassed = 501,
    NotCanisterOrRecipient = 502,
    FloatWithoutCanister = 503,
    EventKeypairNotAllowed = 504,
    JupiterInstructionNotAllowed = 505,

    VaultStateChanged = 600,
    AuthStateChanged = 601,
    WsolState = 602,
    FloatDrawn = 603,
}

impl SwapicError {
    pub const fn code(self) -> u32 {
        self as u32
    }
}
