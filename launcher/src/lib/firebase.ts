import { initializeApp } from "firebase/app";
import {
  getDatabase,
  ref,
  set,
  onValue,
  remove,
  push,
  get,
  onDisconnect,
  serverTimestamp,
  type DatabaseReference,
} from "firebase/database";

const firebaseConfig = {
  apiKey: "AIzaSyCAoMG5eIkGx0t9ifrRvXPvJ92LrQ9sm9E",
  authDomain: "howlingwind-1f319.firebaseapp.com",
  databaseURL: "https://howlingwind-1f319-default-rtdb.firebaseio.com",
  projectId: "howlingwind-1f319",
  storageBucket: "howlingwind-1f319.firebasestorage.app",
  messagingSenderId: "1057831809263",
  appId: "1:1057831809263:web:60518211e46a07964f2b1a",
};

let app: ReturnType<typeof initializeApp> | null = null;
let db: ReturnType<typeof getDatabase> | null = null;

export function initFirebase(config?: typeof firebaseConfig) {
  const cfg = config || firebaseConfig;
  if (!cfg.databaseURL) {
    console.warn("Firebase not configured — lobby features disabled");
    return null;
  }
  app = initializeApp(cfg);
  db = getDatabase(app);
  return db;
}

export function getDb() {
  return db;
}

// ── Lobby Types ──

export interface LobbyPlayer {
  name: string;
  status: "idle" | "in_lobby" | "in_match";
  lastSeen: number | object; // serverTimestamp
}

export interface LobbyRoom {
  host: string;
  hostName: string;
  code: string;
  status: "waiting" | "full" | "playing";
  created: number | object;
  guest?: string;
  guestName?: string;
}

// ── Room Code Generation ──

function generateRoomCode(): string {
  const chars = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789"; // no I/O/0/1 for clarity
  let code = "";
  for (let i = 0; i < 6; i++) {
    code += chars[Math.floor(Math.random() * chars.length)];
  }
  return code;
}

// ── Presence ──

export function setPresence(playerId: string, playerName: string) {
  if (!db) return;
  const playerRef = ref(db, `players/${playerId}`);
  set(playerRef, {
    name: playerName,
    status: "idle",
    lastSeen: serverTimestamp(),
  });
  // Auto-remove on disconnect
  onDisconnect(playerRef).remove();
}

export function removePresence(playerId: string) {
  if (!db) return;
  remove(ref(db, `players/${playerId}`));
}

// ── Rooms ──

export async function createRoom(
  hostId: string,
  hostName: string
): Promise<{ roomId: string; code: string }> {
  if (!db) throw new Error("Firebase not initialized");

  const code = generateRoomCode();
  const roomRef = push(ref(db, "rooms"));
  const roomId = roomRef.key!;

  await set(roomRef, {
    host: hostId,
    hostName,
    code,
    status: "waiting",
    created: serverTimestamp(),
  });

  // Clean up room if host disconnects
  onDisconnect(roomRef).remove();

  // Update player status
  set(ref(db, `players/${hostId}/status`), "in_lobby");

  return { roomId, code };
}

export async function joinRoom(
  code: string,
  guestId: string,
  guestName: string
): Promise<{ roomId: string; room: LobbyRoom } | null> {
  if (!db) throw new Error("Firebase not initialized");

  // Find room by code
  const roomsSnap = await get(ref(db, "rooms"));
  if (!roomsSnap.exists()) return null;

  const rooms = roomsSnap.val();
  for (const [roomId, room] of Object.entries(rooms) as [string, any][]) {
    if (room.code === code.toUpperCase() && room.status === "waiting") {
      // Join the room
      await set(ref(db, `rooms/${roomId}/guest`), guestId);
      await set(ref(db, `rooms/${roomId}/guestName`), guestName);
      await set(ref(db, `rooms/${roomId}/status`), "full");
      await set(ref(db, `players/${guestId}/status`), "in_lobby");

      return { roomId, room: { ...room, guest: guestId, guestName, status: "full" } };
    }
  }

  return null; // Room not found or full
}

export function leaveRoom(roomId: string, playerId: string, isHost: boolean) {
  if (!db) return;
  if (isHost) {
    // Host leaving destroys the room
    remove(ref(db, `rooms/${roomId}`));
  } else {
    // Guest leaving opens the room back up
    set(ref(db, `rooms/${roomId}/guest`), null);
    set(ref(db, `rooms/${roomId}/guestName`), null);
    set(ref(db, `rooms/${roomId}/status`), "waiting");
  }
  set(ref(db, `players/${playerId}/status`), "idle");
}

// ── Listeners ──

export function onRoomsChanged(callback: (rooms: Record<string, LobbyRoom>) => void) {
  if (!db) return () => {};
  return onValue(ref(db, "rooms"), (snapshot) => {
    callback(snapshot.val() || {});
  });
}

export function onRoomChanged(roomId: string, callback: (room: LobbyRoom | null) => void) {
  if (!db) return () => {};
  return onValue(ref(db, `rooms/${roomId}`), (snapshot) => {
    callback(snapshot.val());
  });
}

export function onPlayersChanged(callback: (players: Record<string, LobbyPlayer>) => void) {
  if (!db) return () => {};
  return onValue(ref(db, "players"), (snapshot) => {
    callback(snapshot.val() || {});
  });
}

// ── WebRTC Signaling via Firebase ──

export async function sendSignal(roomId: string, fromId: string, signal: any) {
  if (!db) return;
  const signalRef = push(ref(db, `signals/${roomId}`));
  await set(signalRef, {
    from: fromId,
    signal,
    timestamp: serverTimestamp(),
  });
}

export function onSignals(
  roomId: string,
  myId: string,
  callback: (signal: any, fromId: string) => void
) {
  if (!db) return () => {};
  return onValue(ref(db, `signals/${roomId}`), (snapshot) => {
    const signals = snapshot.val();
    if (!signals) return;
    for (const [key, data] of Object.entries(signals) as [string, any][]) {
      if (data.from !== myId) {
        callback(data.signal, data.from);
        // Clean up processed signal
        remove(ref(db, `signals/${roomId}/${key}`));
      }
    }
  });
}

export function cleanupSignals(roomId: string) {
  if (!db) return;
  remove(ref(db, `signals/${roomId}`));
}

// ── Matchmaking Queue ──

/**
 * Join the matchmaking queue. Firebase will pair two queued players automatically.
 * When a match is found, both players get notified via their queue entry.
 */
export async function joinMatchmakingQueue(
  playerId: string,
  playerName: string,
  game: string = "gnt4"
): Promise<string> {
  if (!db) throw new Error("Firebase not initialized");

  // Check if anyone else is already queued
  const queueSnap = await get(ref(db, `queue/${game}`));
  const queue = queueSnap.val() || {};

  // Find another player waiting (not us)
  for (const [queuedId, data] of Object.entries(queue) as [string, any][]) {
    if (queuedId !== playerId && data.status === "waiting") {
      // Match found! Create a room for both players
      const code = generateRoomCode();
      const roomRef = push(ref(db, "rooms"));
      const roomId = roomRef.key!;

      await set(roomRef, {
        host: queuedId,
        hostName: data.name,
        guest: playerId,
        guestName: playerName,
        code,
        status: "full",
        created: serverTimestamp(),
        matchmade: true,
      });

      // Notify the other player
      await set(ref(db, `queue/${game}/${queuedId}`), {
        ...data,
        status: "matched",
        roomId,
        opponent: playerName,
      });

      // Remove ourselves from queue
      await remove(ref(db, `queue/${game}/${playerId}`));

      // Update statuses
      await set(ref(db, `players/${playerId}/status`), "in_lobby");
      await set(ref(db, `players/${queuedId}/status`), "in_lobby");

      return roomId;
    }
  }

  // No match found — add ourselves to queue
  await set(ref(db, `queue/${game}/${playerId}`), {
    name: playerName,
    status: "waiting",
    joinedAt: serverTimestamp(),
  });

  // Auto-remove from queue on disconnect
  onDisconnect(ref(db, `queue/${game}/${playerId}`)).remove();

  return ""; // Empty string means we're waiting
}

/**
 * Listen for matchmaking result (when someone pairs with us).
 */
export function onMatchFound(
  playerId: string,
  game: string,
  callback: (roomId: string, opponentName: string) => void
) {
  if (!db) return () => {};
  return onValue(ref(db, `queue/${game}/${playerId}`), (snapshot) => {
    const data = snapshot.val();
    if (data && data.status === "matched" && data.roomId) {
      callback(data.roomId, data.opponent);
      // Clean up our queue entry
      remove(ref(db, `queue/${game}/${playerId}`));
    }
  });
}

/**
 * Leave the matchmaking queue.
 */
export function leaveMatchmakingQueue(playerId: string, game: string = "gnt4") {
  if (!db) return;
  remove(ref(db, `queue/${game}/${playerId}`));
}

// ── Friends System ──

export interface FriendEntry {
  name: string;
  addedAt: number | object;
}

export interface FriendRequest {
  fromId: string;
  fromName: string;
  timestamp: number | object;
}

/**
 * Send a friend request to another player.
 */
export async function sendFriendRequest(myId: string, myName: string, targetId: string) {
  if (!db) return;
  // Don't send if already friends
  const existingSnap = await get(ref(db, `friends/${myId}/${targetId}`));
  if (existingSnap.exists()) return;
  // Don't duplicate requests
  const existingReqSnap = await get(ref(db, `friendRequests/${targetId}/${myId}`));
  if (existingReqSnap.exists()) return;

  await set(ref(db, `friendRequests/${targetId}/${myId}`), {
    fromId: myId,
    fromName: myName,
    timestamp: serverTimestamp(),
  });
}

/**
 * Accept a friend request — creates two-way friendship.
 */
export async function acceptFriendRequest(myId: string, myName: string, fromId: string, fromName: string) {
  if (!db) return;
  // Create two-way friendship
  await set(ref(db, `friends/${myId}/${fromId}`), {
    name: fromName,
    addedAt: serverTimestamp(),
  });
  await set(ref(db, `friends/${fromId}/${myId}`), {
    name: myName,
    addedAt: serverTimestamp(),
  });
  // Remove the request
  await remove(ref(db, `friendRequests/${myId}/${fromId}`));
}

/**
 * Reject a friend request.
 */
export async function rejectFriendRequest(myId: string, fromId: string) {
  if (!db) return;
  await remove(ref(db, `friendRequests/${myId}/${fromId}`));
}

/**
 * Listen for incoming friend requests.
 */
export function onFriendRequests(
  myId: string,
  callback: (requests: FriendRequest[]) => void
) {
  if (!db) return () => {};
  return onValue(ref(db, `friendRequests/${myId}`), (snapshot) => {
    const data = snapshot.val() || {};
    const requests = Object.values(data) as FriendRequest[];
    callback(requests);
  });
}

/**
 * Legacy addFriend — now sends a request instead.
 */
export async function addFriend(myId: string, friendId: string, friendName: string) {
  if (!db) return;
  const mySnap = await get(ref(db, `players/${myId}/name`));
  const myName = mySnap.val() || "Unknown";
  await sendFriendRequest(myId, myName, friendId);
}

/**
 * Remove a friend.
 */
export async function removeFriend(myId: string, friendId: string) {
  if (!db) return;
  await remove(ref(db, `friends/${myId}/${friendId}`));
  await remove(ref(db, `friends/${friendId}/${myId}`));
}

/**
 * Listen for friends list changes. Returns friends with online status.
 */
export function onFriendsChanged(
  myId: string,
  callback: (friends: Array<{ id: string; name: string; online: boolean; status: string }>) => void
) {
  if (!db) return () => {};

  // Listen to friends list
  return onValue(ref(db, `friends/${myId}`), async (friendsSnap) => {
    const friendsData = friendsSnap.val() || {};
    const friendIds = Object.keys(friendsData);

    if (friendIds.length === 0) {
      callback([]);
      return;
    }

    // Check each friend's online status
    const playersSnap = await get(ref(db, "players"));
    const players = playersSnap.val() || {};

    const friends = friendIds.map((id) => ({
      id,
      name: friendsData[id].name,
      online: !!players[id],
      status: players[id]?.status || "offline",
    }));

    // Sort: online first, then by name
    friends.sort((a, b) => {
      if (a.online !== b.online) return a.online ? -1 : 1;
      return a.name.localeCompare(b.name);
    });

    callback(friends);
  });
}

/**
 * Send a challenge to a friend. They'll see a notification.
 */
export async function sendChallenge(
  fromId: string,
  fromName: string,
  toId: string,
  game: string = "gnt4"
) {
  if (!db) return;
  await set(ref(db, `challenges/${toId}/${fromId}`), {
    from: fromId,
    fromName: fromName,
    game,
    timestamp: serverTimestamp(),
  });
  // Auto-expire challenge after 60 seconds
  setTimeout(() => {
    if (db) remove(ref(db, `challenges/${toId}/${fromId}`));
  }, 60000);
}

/**
 * Listen for incoming challenges.
 */
export function onChallenges(
  myId: string,
  callback: (challenges: Array<{ fromId: string; fromName: string; game: string }>) => void
) {
  if (!db) return () => {};
  return onValue(ref(db, `challenges/${myId}`), (snapshot) => {
    const data = snapshot.val() || {};
    const challenges = Object.values(data) as Array<{ from: string; fromName: string; game: string }>;
    callback(challenges.map((c) => ({ fromId: c.from, fromName: c.fromName, game: c.game })));
  });
}

/**
 * Accept a challenge — creates a room and notifies the challenger.
 */
export async function acceptChallenge(
  myId: string,
  myName: string,
  challengerId: string
): Promise<string> {
  if (!db) throw new Error("Firebase not initialized");

  const code = generateRoomCode();
  const roomRef = push(ref(db, "rooms"));
  const roomId = roomRef.key!;

  await set(roomRef, {
    host: challengerId,
    hostName: (await get(ref(db, `players/${challengerId}/name`))).val() || "Player",
    guest: myId,
    guestName: myName,
    code,
    status: "full",
    created: serverTimestamp(),
    fromChallenge: true,
  });

  // Notify challenger via signal
  await sendSignal(roomId, myId, { type: "challenge_accepted", roomId });

  // Clean up challenge
  await remove(ref(db, `challenges/${myId}/${challengerId}`));

  // Update statuses
  await set(ref(db, `players/${myId}/status`), "in_lobby");
  await set(ref(db, `players/${challengerId}/status`), "in_lobby");

  return roomId;
}

/**
 * Decline a challenge.
 */
export async function declineChallenge(myId: string, challengerId: string) {
  if (!db) return;
  await remove(ref(db, `challenges/${myId}/${challengerId}`));
}

// ── ELO Rating System ──

export interface PlayerRating {
  name: string;
  elo: number;
  wins: number;
  losses: number;
  streak: number; // positive = win streak, negative = loss streak
  peakElo: number;
  lastPlayed: number | object;
  game: string; // "gnt4" or "gntsp"
}

export interface MatchResult {
  winnerId: string;
  loserId: string;
  winnerName: string;
  loserName: string;
  winnerElo: number;
  loserElo: number;
  eloDelta: number;
  game: string;
  timestamp: number | object;
}

const DEFAULT_ELO = 1200;
const K_FACTOR = 32; // standard K-factor, higher = more volatile

/**
 * Calculate ELO change for a match.
 * Returns positive delta for winner.
 */
export function calculateEloDelta(winnerElo: number, loserElo: number): number {
  const expectedWin = 1 / (1 + Math.pow(10, (loserElo - winnerElo) / 400));
  return Math.round(K_FACTOR * (1 - expectedWin));
}

/**
 * Get or create a player's rating profile.
 */
export async function getPlayerRating(
  playerId: string,
  game: string
): Promise<PlayerRating | null> {
  if (!db) return null;
  const snap = await get(ref(db, `ratings/${game}/${playerId}`));
  return snap.exists() ? snap.val() : null;
}

/**
 * Initialize a player's rating if they don't have one.
 */
export async function ensurePlayerRating(
  playerId: string,
  playerName: string,
  game: string
): Promise<PlayerRating> {
  if (!db) throw new Error("Firebase not initialized");
  const existing = await getPlayerRating(playerId, game);
  if (existing) {
    // Update name if it changed (ELO tracked by ID, name is just display)
    if (existing.name !== playerName) {
      await set(ref(db, `ratings/${game}/${playerId}/name`), playerName);
      existing.name = playerName;
    }
    return existing;
  }

  const rating: PlayerRating = {
    name: playerName,
    elo: DEFAULT_ELO,
    wins: 0,
    losses: 0,
    streak: 0,
    peakElo: DEFAULT_ELO,
    lastPlayed: serverTimestamp(),
    game,
  };

  await set(ref(db, `ratings/${game}/${playerId}`), rating);
  return rating;
}

/**
 * Record a match result and update both players' ELO.
 * Returns the ELO delta applied.
 */
export async function recordMatchResult(
  winnerId: string,
  winnerName: string,
  loserId: string,
  loserName: string,
  game: string
): Promise<number> {
  if (!db) throw new Error("Firebase not initialized");

  // Ensure both players have ratings
  const winnerRating = await ensurePlayerRating(winnerId, winnerName, game);
  const loserRating = await ensurePlayerRating(loserId, loserName, game);

  const delta = calculateEloDelta(winnerRating.elo, loserRating.elo);

  const newWinnerElo = winnerRating.elo + delta;
  const newLoserElo = Math.max(100, loserRating.elo - delta); // floor at 100

  // Update winner
  await set(ref(db, `ratings/${game}/${winnerId}`), {
    name: winnerName,
    elo: newWinnerElo,
    wins: winnerRating.wins + 1,
    losses: winnerRating.losses,
    streak: winnerRating.streak > 0 ? winnerRating.streak + 1 : 1,
    peakElo: Math.max(winnerRating.peakElo, newWinnerElo),
    lastPlayed: serverTimestamp(),
    game,
  });

  // Update loser
  await set(ref(db, `ratings/${game}/${loserId}`), {
    name: loserName,
    elo: newLoserElo,
    wins: loserRating.wins,
    losses: loserRating.losses + 1,
    streak: loserRating.streak < 0 ? loserRating.streak - 1 : -1,
    peakElo: loserRating.peakElo,
    lastPlayed: serverTimestamp(),
    game,
  });

  // Record match history
  const matchResult: MatchResult = {
    winnerId,
    loserId,
    winnerName,
    loserName,
    winnerElo: newWinnerElo,
    loserElo: newLoserElo,
    eloDelta: delta,
    game,
    timestamp: serverTimestamp(),
  };
  await push(ref(db, `matches/${game}`), matchResult);

  return delta;
}

/**
 * Get leaderboard (top players sorted by ELO).
 */
export async function getLeaderboard(
  game: string
): Promise<Array<PlayerRating & { id: string }>> {
  if (!db) return [];
  const snap = await get(ref(db, `ratings/${game}`));
  if (!snap.exists()) return [];

  const ratings = snap.val();
  return Object.entries(ratings)
    .map(([id, rating]: [string, any]) => ({ id, ...rating }))
    .sort((a, b) => b.elo - a.elo);
}

/**
 * Get recent match history.
 */
export async function getMatchHistory(
  game: string,
  limit = 20
): Promise<MatchResult[]> {
  if (!db) return [];
  const snap = await get(ref(db, `matches/${game}`));
  if (!snap.exists()) return [];

  const matches = snap.val();
  return Object.values(matches)
    .sort((a: any, b: any) => (b.timestamp || 0) - (a.timestamp || 0))
    .slice(0, limit) as MatchResult[];
}

/**
 * Listen for leaderboard changes in real-time.
 */
export function onLeaderboardChanged(
  game: string,
  callback: (leaderboard: Array<PlayerRating & { id: string }>) => void
) {
  if (!db) return () => {};
  return onValue(ref(db, `ratings/${game}`), (snapshot) => {
    const ratings = snapshot.val() || {};
    const leaderboard = Object.entries(ratings)
      .map(([id, rating]: [string, any]) => ({ id, ...rating }))
      .sort((a, b) => b.elo - a.elo);
    callback(leaderboard);
  });
}

// ── Admin Functions ──

const ADMIN_ID = "admin_howlingwind"; // Your admin identifier

/**
 * Check if a player ID is the admin.
 */
export function isAdmin(playerId: string): boolean {
  return playerId === ADMIN_ID;
}

/**
 * Admin: manually set a player's ELO.
 */
export async function adminSetElo(
  playerId: string,
  game: string,
  newElo: number
): Promise<void> {
  if (!db) throw new Error("Firebase not initialized");
  const rating = await getPlayerRating(playerId, game);
  if (!rating) throw new Error("Player not found");
  await set(ref(db, `ratings/${game}/${playerId}/elo`), newElo);
  if (newElo > rating.peakElo) {
    await set(ref(db, `ratings/${game}/${playerId}/peakElo`), newElo);
  }
}

/**
 * Admin: reset a player's stats entirely.
 */
export async function adminResetPlayer(
  playerId: string,
  game: string
): Promise<void> {
  if (!db) throw new Error("Firebase not initialized");
  const rating = await getPlayerRating(playerId, game);
  if (!rating) throw new Error("Player not found");
  await set(ref(db, `ratings/${game}/${playerId}`), {
    name: rating.name,
    elo: DEFAULT_ELO,
    wins: 0,
    losses: 0,
    streak: 0,
    peakElo: DEFAULT_ELO,
    lastPlayed: serverTimestamp(),
    game,
  });
}

/**
 * Admin: delete a match from history and reverse the ELO change.
 */
export async function adminDeleteMatch(
  matchId: string,
  game: string
): Promise<void> {
  if (!db) throw new Error("Firebase not initialized");
  const snap = await get(ref(db, `matches/${game}/${matchId}`));
  if (!snap.exists()) throw new Error("Match not found");

  const match: MatchResult = snap.val();

  // Reverse ELO changes
  const winnerRating = await getPlayerRating(match.winnerId, game);
  const loserRating = await getPlayerRating(match.loserId, game);

  if (winnerRating) {
    await set(ref(db, `ratings/${game}/${match.winnerId}/elo`), winnerRating.elo - match.eloDelta);
    await set(ref(db, `ratings/${game}/${match.winnerId}/wins`), Math.max(0, winnerRating.wins - 1));
  }
  if (loserRating) {
    await set(ref(db, `ratings/${game}/${match.loserId}/elo`), loserRating.elo + match.eloDelta);
    await set(ref(db, `ratings/${game}/${match.loserId}/losses`), Math.max(0, loserRating.losses - 1));
  }

  // Delete the match record
  await remove(ref(db, `matches/${game}/${matchId}`));
}
