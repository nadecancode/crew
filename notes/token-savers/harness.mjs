import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

export const root = process.env.CREW_FILTER_SCRATCH;
const tokenizer = process.env.CREW_FILTER_TOKENIZER;
if (!root || !tokenizer) throw new Error('Run through run.mjs');
if (JSON.parse(fs.readFileSync(path.join(tokenizer, 'package.json'))).version !== '1.0.21') {
  throw new Error('Expected js-tiktoken 1.0.21');
}
export const { getEncoding } = await import(pathToFileURL(path.join(tokenizer, 'dist/index.js')));
