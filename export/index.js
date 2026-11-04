import TaskEscrowAbi from './abis/TaskEscrow.json' with {type: 'json'};
import StakeVaultAbi from './abis/StakeVault.json' with {type: 'json'};
import AgentAccountAbi from './abis/AgentAccount.json' with {type: 'json'};
import AgentAccountFactoryAbi from './abis/AgentAccountFactory.json' with {type: 'json'};
import ClientReputationWriterAbi from './abis/ClientReputationWriter.json' with {type: 'json'};
import SettlementValidatorAbi from './abis/SettlementValidator.json' with {type: 'json'};
import AgentDelegateAbi from './abis/AgentDelegate.json' with {type: 'json'};
import MockUSDCAbi from './abis/MockUSDC.json' with {type: 'json'};
import MockIdentityRegistryAbi from './abis/MockIdentityRegistry.json' with {type: 'json'};
import MockReputationRegistryAbi from './abis/MockReputationRegistry.json' with {type: 'json'};
import networks from './networks.json' with {type: 'json'};

export {TaskEscrowAbi, StakeVaultAbi, AgentAccountAbi, AgentAccountFactoryAbi, ClientReputationWriterAbi, SettlementValidatorAbi, AgentDelegateAbi, MockUSDCAbi, MockIdentityRegistryAbi, MockReputationRegistryAbi};
export {networks};

export const deployedChainIds = [10143,31337];
