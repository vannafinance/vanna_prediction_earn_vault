// Address lookup tables for merges. A merge names ~26 fixed accounts plus Jupiter's route, which only fits a v0
// transaction when the fixed ones come from a table. The keeper keeps one table per pool, creates it on first use and
// records it in its state file.

import * as fs from "node:fs";
import {
  AddressLookupTableAccount,
  AddressLookupTableProgram,
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  sendAndConfirmTransaction,
} from "@solana/web3.js";
import { log } from "./log.ts";

/** Addresses per extend instruction, so create + first extend fit one legacy transaction. */
const EXTEND_CHUNK = 20;

export class LookupTables {
  private tables: Record<string, string>;

  constructor(
    private readonly connection: Connection,
    private readonly payer: Keypair,
    private readonly stateFile: string,
  ) {
    this.tables = fs.existsSync(stateFile) ? JSON.parse(fs.readFileSync(stateFile, "utf8")).lookupTables ?? {} : {};
  }

  /** A table holding every address in `addresses`, active and ready to use. */
  async forPool(pool: PublicKey, addresses: PublicKey[]): Promise<AddressLookupTableAccount> {
    const known = this.tables[pool.toBase58()];
    if (known) {
      const table = (await this.connection.getAddressLookupTable(new PublicKey(known))).value;
      if (table?.isActive()) {
        const missing = addresses.filter((a) => !table.state.addresses.some((t) => t.equals(a)));
        if (missing.length === 0) return table;
        if (table.state.authority?.equals(this.payer.publicKey)) return this.extend(table.key, missing);
      }
      log.warn(`lookup table ${known} for pool ${pool.toBase58()} is unusable; creating a new one`);
    }
    const table = await this.create(addresses);
    this.tables[pool.toBase58()] = table.key.toBase58();
    this.save();
    return table;
  }

  private async create(addresses: PublicKey[]): Promise<AddressLookupTableAccount> {
    const recentSlot = await this.connection.getSlot("finalized");
    const [create, key] = AddressLookupTableProgram.createLookupTable({
      authority: this.payer.publicKey,
      payer: this.payer.publicKey,
      recentSlot,
    });
    const tx = new Transaction().add(create, this.extendIx(key, addresses.slice(0, EXTEND_CHUNK)));
    await sendAndConfirmTransaction(this.connection, tx, [this.payer], { commitment: "confirmed" });
    log.info(`created lookup table ${key.toBase58()}`);
    return this.extend(key, addresses.slice(EXTEND_CHUNK));
  }

  private async extend(key: PublicKey, addresses: PublicKey[]): Promise<AddressLookupTableAccount> {
    for (let i = 0; i < addresses.length; i += EXTEND_CHUNK) {
      const tx = new Transaction().add(this.extendIx(key, addresses.slice(i, i + EXTEND_CHUNK)));
      await sendAndConfirmTransaction(this.connection, tx, [this.payer], { commitment: "confirmed" });
    }
    return this.waitUntilUsable(key);
  }

  private extendIx(key: PublicKey, addresses: PublicKey[]) {
    return AddressLookupTableProgram.extendLookupTable({
      lookupTable: key,
      authority: this.payer.publicKey,
      payer: this.payer.publicKey,
      addresses,
    });
  }

  /** New entries are usable from the slot after the one that added them. */
  private async waitUntilUsable(key: PublicKey): Promise<AddressLookupTableAccount> {
    for (let attempt = 0; attempt < 60; attempt++) {
      const table = (await this.connection.getAddressLookupTable(key)).value;
      if (table && (await this.connection.getSlot("confirmed")) > table.state.lastExtendedSlot) return table;
      await new Promise((r) => setTimeout(r, 500));
    }
    throw new Error(`lookup table ${key.toBase58()} never became usable`);
  }

  private save(): void {
    const state = fs.existsSync(this.stateFile) ? JSON.parse(fs.readFileSync(this.stateFile, "utf8")) : {};
    state.lookupTables = this.tables;
    fs.writeFileSync(this.stateFile, JSON.stringify(state, null, 2) + "\n");
  }
}
