// Tickets screen strings. English and Arabic (written natively); the language follows the Team screen.
import type { HubLifecycle, HubState } from '../ipc';

/** The steps of a ticket's life, in order. "held" is a side state shown after them. */
export const STEP_ORDER: HubLifecycle[] = ['new', 'needs_approval', 'approved', 'reply_sent', 'reply_delivered', 'reply_read', 'awaiting_customer', 'closed'];

function enShort(secs: number | null): string {
  if (secs === null) return '';
  if (secs < 60) return `${secs} s`;
  if (secs < 3600) return `${Math.floor(secs / 60)} min`;
  if (secs < 86400) return `${Math.floor(secs / 3600)} h`;
  return `${Math.floor(secs / 86400)} d`;
}
function arShort(secs: number | null): string {
  if (secs === null) return '';
  if (secs < 60) return `${secs} ث`;
  if (secs < 3600) return `${Math.floor(secs / 60)} د`;
  if (secs < 86400) return `${Math.floor(secs / 3600)} س`;
  return `${Math.floor(secs / 86400)} ي`;
}
export interface TStrings {
  tab: string;
  title: string;
  refresh: string;
  back: string;
  needsApproval: string;
  nothingToApprove: string;
  openTickets: string;
  noTickets: string;
  customer: string;
  owner: string;
  status: string;
  waitingOn: string;
  updated: string;
  fingerprint: string;
  draft: string;
  expires: string;
  expired: string;
  noDraft: string;
  approveBtn: string;
  cannotOpen: string;
  confirmOpen: (host: string) => string;
  open: string;
  cancel: string;
  summary: string;
  timeline: string;
  noTimeline: string;
  noSummary: string;
  needsApprovalTag: string;
  loading: string;
  offline: string;
  tokenMissing: string;
  rejected: string;
  notEnabled: string;
  busy: string;
  approvers: string;
  connecting: string;
  /** `ago` is a finished phrase such as "5 minutes ago". */
  staleNote: (ago: string) => string;
  readOnly: string;
  ago: (secs: number | null) => string;
  inTime: (secs: number) => string;
  badgeTip: (needs: number, open: number) => string;
  unknown: string;
  /** The lifecycle steps, in order, and the side state "held". */
  steps: Record<HubLifecycle, string>;
  stepsLabel: string;
  /** `ago` is a finished phrase such as "5 minutes ago": when the ticket reached its current step. */
  since: (ago: string) => string;
  ackSent: string;
  approvedBy: (who: string) => string;
  /** "Reply delivered, 5 minutes ago" / "Reply failed to send". */
  delivery: (kind: 'ack' | 'reply', status: 'sent' | 'delivered' | 'read' | 'failed', ago: string) => string;
  recentlyClosed: string;
  /** The connection state pill. */
  conn: Record<HubState, string>;
  /** "Updated 12 s ago". */
  updatedAgo: (secs: number | null) => string;
}

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

function enSpan(secs: number): string {
  if (secs < 60) return 'under a minute';
  if (secs < 3600) return plural(Math.floor(secs / 60), 'minute', 'minutes');
  if (secs < 86400) return plural(Math.floor(secs / 3600), 'hour', 'hours');
  return plural(Math.floor(secs / 86400), 'day', 'days');
}

function arSpan(secs: number): string {
  if (secs < 60) return 'أقل من دقيقة';
  if (secs < 3600) return `${Math.floor(secs / 60)} دقيقة`;
  if (secs < 86400) return `${Math.floor(secs / 3600)} ساعة`;
  return `${Math.floor(secs / 86400)} يوم`;
}

export const TS: Record<'en' | 'ar', TStrings> = {
  en: {
    tab: 'Tickets',
    title: 'Tickets',
    refresh: 'Refresh',
    back: 'Back to tickets',
    needsApproval: 'Needs approval',
    nothingToApprove: 'Nothing is waiting for your approval.',
    openTickets: 'Open tickets',
    noTickets: 'No open tickets.',
    customer: 'Customer',
    owner: 'Owner',
    status: 'Status',
    waitingOn: 'Waiting on',
    updated: 'Last update',
    fingerprint: 'Fingerprint',
    draft: 'Draft reply',
    expires: 'Expires',
    expired: 'Expired',
    noDraft: '(the draft is not available yet)',
    approveBtn: 'Open to approve',
    cannotOpen: 'No approval page to open',
    confirmOpen: (h) => `Open ${h} in your browser?`,
    open: 'Open',
    cancel: 'Cancel',
    summary: 'Summary',
    timeline: 'Timeline',
    noTimeline: 'No events yet.',
    noSummary: '(no summary)',
    needsApprovalTag: 'Needs approval',
    loading: 'Loading…',
    offline: 'Offline: hub not reachable',
    tokenMissing: 'No hub token yet. Save it to ~/.claude/secrets/hub-api-token.txt (or set AFO_HUB_TOKEN_FILE).',
    rejected: 'The hub token is missing or wrong.',
    notEnabled: 'The hub is running, but its API is not turned on.',
    busy: 'The hub asked us to slow down. It will try again soon.',
    approvers: 'Can approve',
    connecting: 'Connecting to the hub…',
    staleNote: (a) => `Showing the last good list, from ${a}.`,
    readOnly: 'You approve on the admin page. Nothing is approved from here.',
    ago: (s) => (s === null ? '' : `${enSpan(s)} ago`),
    inTime: (s) => `in ${enSpan(s)}`,
    badgeTip: (n, o) => `${n} ${n === 1 ? 'needs' : 'need'} approval, ${o} open`,
    unknown: '—',
    steps: { new: 'New', needs_approval: 'Needs approval', approved: 'Approved', reply_sent: 'Reply sent', reply_delivered: 'Delivered', reply_read: 'Read', awaiting_customer: 'Awaiting customer', closed: 'Closed', held: 'Held' },
    stepsLabel: 'Ticket progress',
    since: (a) => `Reached this step ${a}`,
    ackSent: 'Ack sent ✓',
    approvedBy: (w) => `Approved by ${w}`,
    delivery: (k, s, a) => `${k === 'ack' ? 'Acknowledgement' : 'Reply'} ${s === 'failed' ? 'failed to send' : s}${a && s !== 'failed' ? `, ${a}` : ''}`,
    recentlyClosed: 'Recently closed',
    conn: { connecting: 'Connecting', connected: 'Live', offline: 'Hub offline', token_missing: 'No token', rejected: 'Token rejected', not_enabled: 'API is off', busy: 'Hub busy' },
    updatedAgo: (s) => (s === null ? 'Not updated yet' : `Updated ${enShort(s)} ago`),
  },
  ar: {
    tab: 'التذاكر',
    title: 'التذاكر',
    refresh: 'تحديث',
    back: 'العودة إلى التذاكر',
    needsApproval: 'بانتظار الموافقة',
    nothingToApprove: 'لا يوجد شيء بانتظار موافقتك.',
    openTickets: 'التذاكر المفتوحة',
    noTickets: 'لا توجد تذاكر مفتوحة.',
    customer: 'العميل',
    owner: 'المسؤول',
    status: 'الحالة',
    waitingOn: 'بانتظار',
    updated: 'آخر تحديث',
    fingerprint: 'البصمة',
    draft: 'مسودة الرد',
    expires: 'تنتهي',
    expired: 'انتهت',
    noDraft: '(المسودة غير متاحة بعد)',
    approveBtn: 'افتح للموافقة',
    cannotOpen: 'لا توجد صفحة موافقة لفتحها',
    confirmOpen: (h) => `فتح ${h} في المتصفح؟`,
    open: 'فتح',
    cancel: 'إلغاء',
    summary: 'الملخص',
    timeline: 'السجل الزمني',
    noTimeline: 'لا توجد أحداث بعد.',
    noSummary: '(بدون ملخص)',
    needsApprovalTag: 'بانتظار الموافقة',
    loading: 'جارٍ التحميل…',
    offline: 'غير متصل: لا يمكن الوصول إلى المركز',
    tokenMissing: 'لا يوجد رمز للمركز بعد. احفظه في ~/.claude/secrets/hub-api-token.txt (أو اضبط AFO_HUB_TOKEN_FILE).',
    rejected: 'رمز المركز مفقود أو خاطئ.',
    notEnabled: 'المركز يعمل لكن واجهته غير مفعّلة.',
    busy: 'طلب المركز التمهّل. ستتم المحاولة قريباً.',
    approvers: 'يمكنه الموافقة',
    connecting: 'جارٍ الاتصال بالمركز…',
    staleNote: (a) => `تظهر آخر قائمة صحيحة، ${a}.`,
    readOnly: 'تتم الموافقة من صفحة الإدارة. لا يتم اعتماد أي شيء من هنا.',
    ago: (s) => (s === null ? '' : `منذ ${arSpan(s)}`),
    inTime: (s) => `بعد ${arSpan(s)}`,
    badgeTip: (n, o) => `${n} بانتظار الموافقة، ${o} مفتوحة`,
    unknown: '—',
    steps: { new: 'جديدة', needs_approval: 'بانتظار الموافقة', approved: 'تمت الموافقة', reply_sent: 'تم إرسال الرد', reply_delivered: 'تم التسليم', reply_read: 'تمت القراءة', awaiting_customer: 'بانتظار العميل', closed: 'مغلقة', held: 'معلّقة' },
    stepsLabel: 'مراحل التذكرة',
    since: (a) => `وصلت لهذه المرحلة ${a}`,
    ackSent: 'تم إرسال الإشعار ✓',
    approvedBy: (w) => `وافق عليها ${w}`,
    delivery: (k, s, a) => {
      const what = k === 'ack' ? 'الإشعار' : 'الرد';
      const state = { sent: 'تم إرساله', delivered: 'تم تسليمه', read: 'تمت قراءته', failed: 'فشل إرساله' }[s];
      return `${what}: ${state}${a && s !== 'failed' ? `، ${a}` : ''}`;
    },
    recentlyClosed: 'أُغلقت مؤخرًا',
    conn: { connecting: 'جارٍ الاتصال', connected: 'مباشر', offline: 'المركز غير متصل', token_missing: 'لا يوجد رمز', rejected: 'الرمز مرفوض', not_enabled: 'الواجهة متوقفة', busy: 'المركز مشغول' },
    updatedAgo: (s) => (s === null ? 'لم يتم التحديث بعد' : `آخر تحديث منذ ${arShort(s)}`),
  },
};
