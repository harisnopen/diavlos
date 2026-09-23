export interface Message {
  v: number; id: string; room: string; seq: number; prev: string;
  trace: string | null; from: string; agent: any; type: string; text: string;
  action: any; data: any; reply_to: string | null; to: string | null;
  class: string; ts: string; sig: string; tombstone?: boolean;
  /** Set when the message is held for you (next with ack: false, messages(), watch()). */
  delivery?: Delivery;
}
export interface Delivery { token: string; lease_until: string; attempt: number; }
export interface SendOptions { type?: string; to?: string; reply_to?: string; trace?: string; data?: any; }
export interface RoomOptions { name?: string; home?: string; }
export class DiavlosError extends Error { code: number; }
export class Room {
  room: string; identity: string; me: string | null;
  static join(invite: string, opts?: RoomOptions): Promise<Room>;
  static open(room: string, opts?: RoomOptions): Room;
  send(text: string, opts?: SendOptions): Promise<Message>;
  ask(text: string, opts?: { timeout?: number; action?: any; trace?: string }): Promise<Message>;
  next(timeout?: number, opts?: { ack?: boolean; lease?: number }): Promise<Message>;
  messages(timeout?: number): AsyncIterable<Message>;
  watch(): AsyncIterable<Message>;
  ack(msg: Message | string): Promise<any>;
  renew(msg: Message | string, lease?: number): Promise<any>;
  nack(msg: Message | string, retryIn?: number): Promise<any>;
  read(since?: number, limit?: number, ack?: boolean): Promise<Message[]>;
  claim(taskId: string): Promise<Message>;
  release(taskId: string): Promise<Message>;
  who(): Promise<any[]>;
}
export function rooms(home?: string): Promise<any[]>;
