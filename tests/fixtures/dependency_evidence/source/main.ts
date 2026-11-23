function tsTarget() {}
export function tsCaller() { tsTarget(); }
import { realTs as tsAlias } from './ts_defs';
export function aliasTsCaller() { tsAlias(); }
