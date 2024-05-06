import { WitnessMap } from '@noir-lang/acvm_js';

// See `complex_brillig_foreign_call` integration test in `acir/tests/test_program_serialization.rs`.
export const bytecode = Uint8Array.from([
  31, 139, 8, 0, 0, 0, 0, 0, 0, 255, 213, 84, 75, 10, 132, 48, 12, 77, 218, 209, 145, 217, 205, 13, 6, 198, 3, 84, 79,
  224, 93, 196, 157, 162, 75, 79, 47, 22, 124, 197, 16, 186, 17, 43, 104, 32, 36, 109, 126, 143, 36, 45, 211, 70, 133,
  103, 134, 110, 61, 27, 232, 140, 179, 164, 224, 215, 64, 186, 115, 84, 113, 186, 92, 238, 42, 140, 230, 1, 24, 237, 5,
  24, 195, 62, 220, 116, 222, 41, 231, 146, 180, 127, 54, 242, 126, 94, 158, 51, 207, 57, 206, 111, 200, 2, 247, 4, 219,
  79, 245, 157, 132, 31, 137, 89, 52, 73, 176, 214, 46, 167, 125, 23, 89, 213, 254, 8, 156, 237, 56, 76, 125, 55, 91,
  229, 170, 161, 254, 133, 94, 42, 59, 171, 184, 69, 197, 46, 66, 202, 47, 40, 86, 39, 220, 155, 3, 185, 191, 180, 183,
  55, 163, 72, 98, 70, 66, 221, 251, 40, 173, 255, 35, 68, 62, 61, 5, 0, 0,
]);
export const initialWitnessMap: WitnessMap = new Map([
  [1, '0x0000000000000000000000000000000000000000000000000000000000000001'],
  [2, '0x0000000000000000000000000000000000000000000000000000000000000002'],
  [3, '0x0000000000000000000000000000000000000000000000000000000000000003'],
]);

export const oracleCallName = 'complex';
export const oracleCallInputs = [
  [
    '0x0000000000000000000000000000000000000000000000000000000000000001',
    '0x0000000000000000000000000000000000000000000000000000000000000002',
    '0x0000000000000000000000000000000000000000000000000000000000000003',
  ],
  ['0x0000000000000000000000000000000000000000000000000000000000000006'],
];

export const oracleResponse = [
  [
    '0x0000000000000000000000000000000000000000000000000000000000000002',
    '0x0000000000000000000000000000000000000000000000000000000000000006',
    '0x000000000000000000000000000000000000000000000000000000000000000c',
  ],
  '0x0000000000000000000000000000000000000000000000000000000000000006',
  '0x000000000000000000000000000000000000000000000000000000000000000c',
];

export const expectedWitnessMap = new Map([
  [1, '0x0000000000000000000000000000000000000000000000000000000000000001'],
  [2, '0x0000000000000000000000000000000000000000000000000000000000000002'],
  [3, '0x0000000000000000000000000000000000000000000000000000000000000003'],
  [4, '0x0000000000000000000000000000000000000000000000000000000000000002'],
  [5, '0x0000000000000000000000000000000000000000000000000000000000000006'],
  [6, '0x000000000000000000000000000000000000000000000000000000000000000c'],
  [7, '0x0000000000000000000000000000000000000000000000000000000000000006'],
  [8, '0x000000000000000000000000000000000000000000000000000000000000000c'],
]);
