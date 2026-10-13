import { beforeEach, describe, expect, mock, test } from 'bun:test';

/**
 * La parte de la interfaz: qué línea va debajo de cada cuenta y que los
 * comandos y eventos del plugin se llamen con lo que el lado de Rust espera.
 * `invoke` y `listen` se reemplazan en su borde (`@tauri-apps/api`).
 */

type Call = { cmd: string; args: unknown };
const calls: Call[] = [];
let reply: unknown = [];
const listeners: Record<string, (event: { payload: unknown }) => void> = {};
const unlisten = () => {};

mock.module('@tauri-apps/api/core', () => ({
	invoke: async (cmd: string, args: unknown) => {
		calls.push({ cmd, args });
		return reply;
	},
}));
mock.module('@tauri-apps/api/event', () => ({
	listen: async (event: string, handler: (event: { payload: unknown }) => void) => {
		listeners[event] = handler;
		return unlisten;
	},
}));

const store = await import('../guest-js/index');
const { syncedWhen, syncLine, requestSync, syncStatus, onStoreChanged, onSyncStatus } = store;
type AccountSyncStatus = import('../guest-js/index').AccountSyncStatus;
type SyncRequestFailure = import('../guest-js/index').SyncRequestFailure;

beforeEach(() => {
	calls.length = 0;
	reply = [];
	for (const key of Object.keys(listeners)) delete listeners[key];
});

// Fechas en hora local: «hoy» es el día de quien mira, no el de UTC.
const now = new Date(2026, 9, 13, 18, 0);
const today = new Date(2026, 9, 13, 9, 5).toISOString();
const yesterday = new Date(2026, 9, 12, 23, 59).toISOString();
const lastYear = new Date(2025, 9, 13, 9, 5).toISOString();

function status(state: string, extra: Partial<AccountSyncStatus> = {}): AccountSyncStatus {
	return { account_id: 'acc-1', state, detail: '', last_synced_at: null, ...extra };
}

function failure(reason: SyncRequestFailure['reason']): SyncRequestFailure {
	return { account_id: 'acc-1', reason };
}

describe('syncedWhen', () => {
	test('si la última vuelta fue hoy dice la hora', () => {
		expect(syncedWhen(today, now, 'en-GB')).toBe('09:05');
	});

	test('si fue otro día dice el día y no la hora', () => {
		expect(syncedWhen(yesterday, now, 'en-GB')).toBe('12 Oct');
	});

	test('el mismo día de otro año no es hoy', () => {
		expect(syncedWhen(lastYear, now, 'en-GB')).toBe('13 Oct');
	});

	test('el mismo día de otro mes no es hoy', () => {
		const lastMonth = new Date(2026, 8, 13, 9, 5);
		const day = new Intl.DateTimeFormat('en-GB', { day: 'numeric', month: 'short' }).format(lastMonth);
		expect(syncedWhen(lastMonth.toISOString(), now, 'en-GB')).toBe(day);
		expect(day).not.toContain('09');
	});

	test('una fecha que no se entiende no dice nada', () => {
		expect(syncedWhen('ayer a la tarde', now)).toBe('');
		expect(syncedWhen('', now)).toBe('');
	});

	test('sigue el idioma que se le pide', () => {
		// El texto exacto depende de los datos de idioma del motor; lo que se
		// cuida es que se pase el idioma y no el del sistema.
		const es = new Intl.DateTimeFormat('es-AR', { day: 'numeric', month: 'short' }).format(
			new Date(yesterday)
		);
		expect(syncedWhen(yesterday, now, 'es-AR')).toBe(es);
		expect(es).not.toBe(syncedWhen(yesterday, now, 'en-GB'));
	});
});

describe('syncLine', () => {
	test('lo que no se pudo pedir va antes que el estado', () => {
		expect(syncLine(status('syncing'), failure('denied'), now)).toEqual({
			tone: 'warning',
			key: 'sync.request.denied',
			arg: '',
			pulse: false,
		});
	});

	test('pedir de nuevo enseguida no es una advertencia', () => {
		expect(syncLine(undefined, failure('too_soon'), now)).toEqual({
			tone: 'neutral',
			key: 'sync.request.too_soon',
			arg: '',
			pulse: false,
		});
	});

	test('sin sincronizador o con un fallo, «Actualizar» lo dice como advertencia', () => {
		for (const reason of ['unavailable', 'failed'] as const) {
			expect(syncLine(status('synced'), failure(reason), now)).toEqual({
				tone: 'warning',
				key: `sync.request.${reason}`,
				arg: '',
				pulse: false,
			});
		}
	});

	test('mientras espera su vuelta el punto late', () => {
		expect(syncLine(status('pending'), undefined, now)).toEqual({
			tone: 'neutral',
			key: 'sync.pending',
			arg: '',
			pulse: true,
		});
	});

	test('mientras sincroniza el punto late con el acento', () => {
		expect(syncLine(status('syncing'), undefined, now)).toEqual({
			tone: 'accent',
			key: 'sync.syncing',
			arg: '',
			pulse: true,
		});
	});

	test('sincronizada con fecha dice cuándo', () => {
		expect(syncLine(status('synced', { last_synced_at: today }), undefined, now, 'en-GB')).toEqual({
			tone: 'success',
			key: 'sync.syncedAt',
			arg: '09:05',
			pulse: false,
		});
	});

	test('sincronizada sin fecha, o con una que no se entiende, no inventa una', () => {
		const sinFecha = { tone: 'success', key: 'sync.synced', arg: '', pulse: false } as const;
		expect(syncLine(status('synced'), undefined, now)).toEqual(sinFecha);
		expect(syncLine(status('synced', { last_synced_at: 'x' }), undefined, now)).toEqual(sinFecha);
	});

	test('un fallo muestra lo que escribió el almacén', () => {
		expect(syncLine(status('failed', { detail: 'sin red' }), undefined, now)).toEqual({
			tone: 'error',
			key: 'sync.failed',
			arg: 'sin red',
			pulse: false,
		});
	});

	test('no disponible se ve como advertencia, nunca como roto', () => {
		expect(syncLine(status('unavailable', { detail: 'sin permiso' }), undefined, now)).toEqual({
			tone: 'warning',
			key: 'sync.unavailable',
			arg: 'sin permiso',
			pulse: false,
		});
	});

	test('un área apagada, desconocida o sin estado no lleva línea', () => {
		expect(syncLine(status('off'), undefined, now)).toBeNull();
		expect(syncLine(status('lo-que-sea'), undefined, now)).toBeNull();
		expect(syncLine(undefined, undefined, now)).toBeNull();
	});
});

describe('comandos', () => {
	test('«Actualizar» pide la vuelta con las cuentas como las espera el comando', async () => {
		reply = [failure('denied')];
		const failures = await requestSync(['a', 'b']);
		expect(calls).toEqual([{ cmd: 'plugin:vsk-store|request_sync', args: { accountIds: ['a', 'b'] } }]);
		expect(failures).toEqual([failure('denied')]);
	});

	test('si «Actualizar» no contesta una lista no hay fallos que mostrar', async () => {
		for (const raro of [null, undefined, {}, 'x', 3]) {
			reply = raro;
			expect(await requestSync(['a'])).toEqual([]);
		}
	});

	test('el estado se pide por área', async () => {
		reply = [status('synced')];
		expect(await syncStatus('calendar')).toEqual([status('synced')]);
		expect(calls).toEqual([{ cmd: 'plugin:vsk-store|sync_status', args: { area: 'calendar' } }]);
	});

	test('si el estado no es una lista no hay cuentas', async () => {
		for (const raro of [null, undefined, {}, 'x']) {
			reply = raro;
			expect(await syncStatus('email')).toEqual([]);
		}
	});
});

describe('avisos', () => {
	test('el aviso de cambios llega con sus áreas', async () => {
		const seen: string[][] = [];
		const stop = await onStoreChanged((areas) => seen.push(areas));
		expect(stop).toBe(unlisten);
		listeners['vsk-store://changed']({ payload: { areas: ['calendar', 'contacts'] } });
		expect(seen).toEqual([['calendar', 'contacts']]);
	});

	test('un aviso de cambios mal formado llega sin áreas', async () => {
		const seen: string[][] = [];
		await onStoreChanged((areas) => seen.push(areas));
		const handler = listeners['vsk-store://changed'];
		handler({ payload: null });
		handler({ payload: {} });
		handler({ payload: { areas: 'calendar' } });
		expect(seen).toEqual([[], [], []]);
	});

	test('el aviso de estado se escucha en su evento', async () => {
		let count = 0;
		const stop = await onSyncStatus(() => count++);
		expect(stop).toBe(unlisten);
		listeners['vsk-store://status']({ payload: null });
		expect(count).toBe(1);
		expect(Object.keys(listeners)).toEqual(['vsk-store://status']);
	});

	test('los nombres de los eventos son los que emite el plugin', () => {
		expect(store.CHANGED_EVENT).toBe('vsk-store://changed');
		expect(store.STATUS_EVENT).toBe('vsk-store://status');
	});
});
