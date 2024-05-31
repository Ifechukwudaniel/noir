import {
  type ProvingJob,
  type ProvingJobSource,
  type ProvingRequest,
  type ProvingRequestResult,
  ProvingRequestType,
  type ServerCircuitProver,
  makePublicInputsAndProof,
} from '@aztec/circuit-types';
import { NESTED_RECURSIVE_PROOF_LENGTH, VerificationKeyData, makeEmptyRecursiveProof } from '@aztec/circuits.js';
import { createDebugLogger } from '@aztec/foundation/log';
import { RunningPromise } from '@aztec/foundation/running-promise';
import { elapsed } from '@aztec/foundation/timer';

import { ProvingError } from './proving-error.js';

/**
 * A helper class that encapsulates a circuit prover and connects it to a job source.
 */
export class ProverAgent {
  private inFlightPromises = new Set<Promise<any>>();
  private runningPromise?: RunningPromise;

  constructor(
    /** The prover implementation to defer jobs to */
    private circuitProver: ServerCircuitProver,
    /** How many proving jobs this agent can handle in parallel */
    private maxConcurrency = 1,
    /** How long to wait between jobs */
    private pollIntervalMs = 100,
    private log = createDebugLogger('aztec:prover-client:prover-agent'),
  ) {}

  setMaxConcurrency(maxConcurrency: number): void {
    if (maxConcurrency < 1) {
      throw new Error('Concurrency must be at least 1');
    }
    this.maxConcurrency = maxConcurrency;
  }

  setCircuitProver(circuitProver: ServerCircuitProver): void {
    this.circuitProver = circuitProver;
  }

  isRunning() {
    return this.runningPromise?.isRunning() ?? false;
  }

  start(jobSource: ProvingJobSource): void {
    if (this.runningPromise) {
      throw new Error('Agent is already running');
    }

    this.runningPromise = new RunningPromise(async () => {
      while (this.inFlightPromises.size < this.maxConcurrency) {
        const job = await jobSource.getProvingJob();
        if (!job) {
          // job source is fully drained, sleep for a bit and try again
          return;
        }

        const promise = this.work(jobSource, job).finally(() => this.inFlightPromises.delete(promise));
        this.inFlightPromises.add(promise);
      }
    }, this.pollIntervalMs);

    this.runningPromise.start();
    this.log.info('Agent started');
  }

  async stop(): Promise<void> {
    if (!this.runningPromise?.isRunning()) {
      return;
    }

    await this.runningPromise.stop();
    this.runningPromise = undefined;

    this.log.info('Agent stopped');
  }

  private async work(jobSource: ProvingJobSource, job: ProvingJob<ProvingRequest>): Promise<void> {
    try {
      const [time, result] = await elapsed(this.getProof(job.request));
      await jobSource.resolveProvingJob(job.id, result);
      this.log.debug(
        `Processed proving job id=${job.id} type=${ProvingRequestType[job.request.type]} duration=${time}ms`,
      );
    } catch (err) {
      this.log.error(`Error processing proving job id=${job.id} type=${ProvingRequestType[job.request.type]}: ${err}`);
      await jobSource.rejectProvingJob(job.id, new ProvingError((err as any)?.message ?? String(err)));
    }
  }

  private getProof(request: ProvingRequest): Promise<ProvingRequestResult<typeof type>> {
    const { type, inputs } = request;
    switch (type) {
      case ProvingRequestType.PUBLIC_VM: {
        return Promise.resolve(
          makePublicInputsAndProof<object>(
            {},
            makeEmptyRecursiveProof(NESTED_RECURSIVE_PROOF_LENGTH),
            VerificationKeyData.makeFake(),
          ),
        );
      }

      case ProvingRequestType.PUBLIC_KERNEL_NON_TAIL: {
        return this.circuitProver.getPublicKernelProof({
          type: request.kernelType,
          inputs,
        });
      }

      case ProvingRequestType.PUBLIC_KERNEL_TAIL: {
        return this.circuitProver.getPublicTailProof({
          type: request.kernelType,
          inputs,
        });
      }

      case ProvingRequestType.BASE_ROLLUP: {
        return this.circuitProver.getBaseRollupProof(inputs);
      }

      case ProvingRequestType.MERGE_ROLLUP: {
        return this.circuitProver.getMergeRollupProof(inputs);
      }

      case ProvingRequestType.ROOT_ROLLUP: {
        return this.circuitProver.getRootRollupProof(inputs);
      }

      case ProvingRequestType.BASE_PARITY: {
        return this.circuitProver.getBaseParityProof(inputs);
      }

      case ProvingRequestType.ROOT_PARITY: {
        return this.circuitProver.getRootParityProof(inputs);
      }

      case ProvingRequestType.PRIVATE_KERNEL_EMPTY: {
        return this.circuitProver.getEmptyPrivateKernelProof(inputs);
      }

      default: {
        const _exhaustive: never = type;
        return Promise.reject(new Error(`Invalid proof request type: ${type}`));
      }
    }
  }
}
