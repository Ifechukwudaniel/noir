#pragma once
#include <stddef.h>
#include "../../constants.hpp"

namespace rollup {
namespace proofs {
namespace notes {

constexpr size_t NOTE_VALUE_BIT_LENGTH = 252;

enum GeneratorIndex {
    JOIN_SPLIT_NULLIFIER_HASH_INPUTS, // encrypt. 4 inputs. 0-7.
    ACCOUNT_NOTE_HASH_INPUTS = 4,     // encrypt. 3 inputs. 8-13.
    ACCOUNT_ALIAS_ID_NULLIFIER = 7,   // compress. 4 inputs. 14-21.
    ACCOUNT_GIBBERISH_NULLIFIER = 11, // compress. 2 inputs. 22-25.

    JOIN_SPLIT_NOTE_OWNER = 13,               // compress_to_point. 26-29.
    JOIN_SPLIT_CLAIM_NOTE_PARTIAL_STATE = 15, // compress_to_point. 30-33.

    JOIN_SPLIT_NOTE_VALUE = 34,
    JOIN_SPLIT_NOTE_SECRET,
    JOIN_SPLIT_NOTE_ASSET_ID,
    JOIN_SPLIT_NOTE_NONCE,
    JOIN_SPLIT_NULLIFIER_ACCOUNT_PRIVATE_KEY,
    JOIN_SPLIT_CLAIM_NOTE_VALUE,
    JOIN_SPLIT_CLAIM_NOTE_BRIDGE_ID,
    JOIN_SPLIT_CLAIM_NOTE_DEFI_INTERACTION_NONCE,
    DEFI_INTERACTION_NOTE_TOTAL_INPUT_VALUE,
    DEFI_INTERACTION_NOTE_BRIDGE_ID,
    DEFI_INTERACTION_NOTE_TOTAL_OUTPUT_A_VALUE,
    DEFI_INTERACTION_NOTE_TOTAL_OUTPUT_B_VALUE,
    DEFI_INTERACTION_NOTE_INTERACTION_NONCE,
    DEFI_INTERACTION_NOTE_INTERACTION_RESULT,
};

constexpr uint32_t DEFI_BRIDGE_ADDRESS_BIT_LENGTH = 160;
constexpr uint32_t DEFI_BRIDGE_NUM_OUTPUT_NOTES_LEN = 2;
constexpr uint32_t DEFI_BRIDGE_INPUT_ASSET_ID_LEN = 32;
constexpr uint32_t DEFI_BRIDGE_OUTPUT_A_ASSET_ID_LEN = 32;
constexpr uint32_t DEFI_BRIDGE_OUTPUT_B_ASSET_ID_LEN = 26;

} // namespace notes
} // namespace proofs
} // namespace rollup