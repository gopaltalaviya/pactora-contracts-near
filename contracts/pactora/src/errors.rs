//! Panic messages. The names match the v3 EVM errors (TaskEscrow, StakeVault,
//! AgentAccountCore) wherever the rule is the same, so the backend maps a
//! refusal to one meaning on both chains.

pub const NOT_OWNER: &str = "NotOwner";
pub const NOT_PENDING_OWNER: &str = "NotPendingOwner";
pub const NOT_ARBITER: &str = "NotArbiter";
pub const NOT_PAUSER: &str = "NotPauser";
pub const PAUSED: &str = "Paused";
pub const WRONG_TOKEN: &str = "WrongToken";
pub const BAD_MSG: &str = "BadMessage";

pub const NO_SUCH_AGENT: &str = "NoSuchAgent";
pub const NOT_AGENT_OWNER: &str = "NotAgentOwner";
pub const NOT_AGENT_WALLET: &str = "NotAgentWallet";
pub const NOT_PENDING_WALLET: &str = "NotPendingWallet";
pub const WALLET_TAKEN: &str = "WalletTaken";
pub const AGENT_FROZEN: &str = "AgentFrozen";
pub const CARD_URI_TOO_LONG: &str = "CardUriTooLong";

pub const ZERO_AMOUNT: &str = "ZeroAmount";
pub const AMOUNT_BELOW_MINIMUM: &str = "AmountBelowMinimum";
pub const AMOUNT_ABOVE_MAXIMUM: &str = "AmountAboveMaximum";
pub const TOO_LARGE_FOR_FAST_PATH: &str = "AmountTooLargeForFastPath";
pub const SELF_DEALING: &str = "SelfDealing";
pub const SAME_OWNER: &str = "SameOwner";
pub const NOT_HIREABLE: &str = "NotHireable";
pub const WINDOW_TOO_LONG: &str = "WindowTooLong";
pub const BAD_HASH: &str = "BadHash";

pub const NO_SUCH_JOB: &str = "NoSuchJob";
pub const INVALID_STATE: &str = "InvalidState";
pub const DEADLINE_PASSED: &str = "DeadlinePassed";
pub const DEADLINE_NOT_PASSED: &str = "DeadlineNotPassed";

pub const INSUFFICIENT_BALANCE: &str = "InsufficientBalance";
pub const PER_TASK_CAP_EXCEEDED: &str = "PerTaskCapExceeded";
pub const DAILY_CAP_EXCEEDED: &str = "DailyCapExceeded";

pub const INSUFFICIENT_BOND: &str = "InsufficientBond";
pub const WITHDRAW_ALREADY_PENDING: &str = "WithdrawAlreadyPending";
pub const NOTHING_PENDING: &str = "NothingPending";
pub const STILL_LOCKED: &str = "StillLocked";
pub const NOTHING_OWED: &str = "NothingOwed";

pub const INSUFFICIENT_STORAGE: &str = "InsufficientStorage";
pub const NOT_ENOUGH_GAS: &str = "NotEnoughGas";

pub const FEE_TOO_HIGH: &str = "FeeTooHigh";
pub const INVALID_WINDOW: &str = "InvalidWindow";
pub const INVALID_FEE_FLOOR: &str = "InvalidFeeFloor";
pub const INVALID_BOND: &str = "InvalidBond";
pub const INVALID_STAKE: &str = "InvalidStake";
pub const INVALID_CAPS: &str = "InvalidCaps";
