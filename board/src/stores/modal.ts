import { writable } from 'svelte/store';

export const modalOpen = writable<boolean>(false);

export function openModal(): void { modalOpen.set(true); }
export function closeModal(): void { modalOpen.set(false); }
