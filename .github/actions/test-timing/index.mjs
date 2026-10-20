import { run } from './timing.mjs';

try {
  run(process.env);
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
