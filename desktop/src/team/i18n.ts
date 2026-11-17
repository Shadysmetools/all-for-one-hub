// Team board strings, ported from the approved mockup. Arabic is written natively.
export type Lang = 'en' | 'ar';

export interface Strings {
  tabTeam: string;
  tabOne: string;
  chatSend: string;
  chatPlaceholder: string;
  chatHint: string;
  talk: string;
  micOnStop: string;
  stop: string;
  testVoice: string;
  more: string;
  askSession: string;
  pillOff: string;
  pillListening: string;
  pillSpeaking: string;
  pillThinking: string;
  greeting: string;
  thinking: string;
  stillWorking: string;
  shrink: string;
  title: string;
  sub: string;
  light: string;
  dark: string;
  mine: string;
  team: string;
  cards: string;
  graph: string;
  helpers: string;
  waitYou: string;
  waitOthers: string;
  nothing: string;
  big: (n: number) => string;
  stale: (n: number) => string;
  allClear: string;
  /** "Hub tickets need you: 2". */
  hubNeeds: (n: number) => string;
  hubMore: (n: number) => string;
  hubOpenTickets: string;
  /** Said instead of "all caught up" when the hub cannot be asked. */
  hubOffline: string;
  hubTokenMissing: string;
  hubChecking: string;
  hubOther: string;
  /** Session routing: which session the default chat uses, and why. */
  routeDefault: (title: string) => string;
  routeCoordinator: (title: string) => string;
  routeAfo: (title: string) => string;
  routePin: (title: string) => string;
  routePinGone: string;
  routeNone: string;
  pin: string;
  unpin: string;
  working: string;
  onThem: string;
  onOthers: string;
  show: string;
  hide: string;
  done: string;
  remind: string;
  reminded: string;
  autoSent: string;
  ask: string;
  askAbout: string;
  undo: string;
  markedDone: string;
  remSent: (n: string) => string;
  access: string;
  approval: string;
  ticket: string;
  long: string;
  who: string;
  whoSub: string;
  list: string;
  listSub: string;
  you: string;
  active: string;
  waiting: string;
  projects: string;
  projectRole: string;
  sessionRole: string;
  emptyBoard: string;
  emptyBoardSub: string;
  emptyTitle: string;
  emptyBody: string;
  emptySample: string;
  errTitle: string;
  errBody: string;
  skipped: (n: number) => string;
  loading: string;
  needs: string;
  needsF: string;
  needYou: string;
  toWord: string;
  forWord: string;
  toYou: string;
  ageF: (h: number) => string;
}

export const T: Record<Lang, Strings> = {
  en: {
    tabTeam: 'Team',
    tabOne: 'Chat with One',
    chatSend: 'Send',
    chatPlaceholder: 'Type your message here',
    chatHint: 'Enter sends. Shift+Enter starts a new line.',
    talk: 'Talk to One',
    micOnStop: 'Mic on: tap to stop',
    stop: 'Stop',
    testVoice: 'Test my voice',
    more: 'Sound and more',
    askSession: 'Ask about a teammate\'s work',
    pillOff: 'Mic off',
    pillListening: 'Listening',
    pillSpeaking: 'Speaking',
    pillThinking: 'Thinking',
    greeting: 'Hi, I\'m One. Type below, or tap **Talk to One** to speak to me.',
    thinking: 'One is thinking…',
    stillWorking: 'One is still working on it…',
    shrink: 'Small',
    title: 'Team',
    sub: 'What is waiting on you, and on others',
    light: 'Light',
    dark: 'Dark',
    mine: 'My asks',
    team: 'Everyone',
    cards: 'People and agents',
    graph: 'Who needs whom',
    helpers: 'Show helpers',
    waitYou: 'Waiting on you',
    waitOthers: 'You are waiting on',
    nothing: 'Nothing here. All clear.',
    big: (n) => `${n} ${n === 1 ? 'thing is' : 'things are'} waiting on you`,
    stale: (n) => `${n} older than 2 days`,
    allClear: 'You are all caught up',
    hubNeeds: (n) => `Hub tickets need you: ${n}`,
    hubMore: (n) => `and ${n} more`,
    hubOpenTickets: 'Open the Tickets screen',
    hubOffline: 'Cannot reach the hub, so I cannot tell if tickets are waiting for you.',
    hubTokenMissing: 'No hub token yet, so I cannot check the hub tickets.',
    hubChecking: 'Checking the hub…',
    hubOther: 'The hub is not answering properly, so I cannot check its tickets.',
    routeDefault: (t) => `All for One (current): ${t}`,
    routeCoordinator: (t) => `Suggested to ask about: ${t} (the coordinator)`,
    routeAfo: (t) => `The coordinator is not available to One (not live, or in another project), so suggesting the newest live All for One session: ${t}`,
    routePin: (t) => `Pinned: ${t}`,
    routePinGone: 'The pinned session is no longer live, so the suggestion is used.',
    routeNone: 'No live All for One session',
    pin: 'Pin',
    unpin: 'Unpin',
    working: 'Working on',
    onThem: 'waiting on them',
    onOthers: 'they are waiting',
    show: 'Show details',
    hide: 'Hide details',
    done: 'Done',
    remind: 'Remind',
    reminded: 'Reminded',
    autoSent: 'Reminder sent',
    ask: 'Ask One',
    askAbout: 'Ask One about this',
    undo: 'Undo',
    markedDone: 'Marked done',
    remSent: (n) => `Reminder sent to ${n}`,
    access: 'Access',
    approval: 'Approval',
    ticket: 'Ticket',
    long: 'Waiting long',
    who: 'Who is everyone waiting on?',
    whoSub: 'Number of open asks per person or agent.',
    list: 'Every open ask',
    listSub: 'The one who needs something, an arrow, the one who has it.',
    you: 'you',
    active: 'Working',
    waiting: 'Waiting on someone',
    projects: 'What the team is working on',
    projectRole: 'Project',
    sessionRole: 'Session',
    emptyBoard: 'Nothing waiting on anyone \u{1F389}',
    emptyBoardSub: 'When a session or a teammate needs something, it shows up here by itself.',
    emptyTitle: 'Nothing on the board yet',
    emptyBody: 'The board fills itself as your team and sessions work. Check back in a minute.',
    emptySample: '',
    errTitle: "Part of the board couldn't be read",
    errBody: 'One will try again by itself. If this stays, ask the team to check the shared team list.',
    skipped: (n) => `${n} ${n === 1 ? 'entry was' : 'entries were'} skipped (private or not valid).`,
    loading: 'Loading...',
    needs: 'needs',
    needsF: 'need',
    needYou: 'You',
    toWord: '',
    forWord: 'for',
    toYou: 'you',
    ageF: (h) => {
      if (h < 1) return 'just now';
      if (h < 24) return `${h} ${h === 1 ? 'hour' : 'hours'}`;
      const d = Math.round(h / 24);
      return `${d} ${d === 1 ? 'day' : 'days'}`;
    },
  },
  ar: {
    tabTeam: 'الفريق',
    tabOne: 'تحدث مع One',
    chatSend: 'إرسال',
    chatPlaceholder: 'اكتب رسالتك هنا',
    chatHint: 'Enter للإرسال، و Shift+Enter لسطر جديد.',
    talk: 'تحدّث مع One',
    micOnStop: 'الميكروفون يعمل: اضغط للإيقاف',
    stop: 'إيقاف',
    testVoice: 'جرّب صوتي',
    more: 'الصوت والمزيد',
    askSession: 'اسأل عن عمل زميل',
    pillOff: 'الميكروفون مغلق',
    pillListening: 'أستمع',
    pillSpeaking: 'أتحدث',
    pillThinking: 'أفكر',
    greeting: 'أهلًا، أنا One. اكتب بالأسفل، أو اضغط **تحدّث مع One** لتتحدث معي.',
    thinking: 'One يفكر…',
    stillWorking: 'One ما زال يعمل على ذلك…',
    shrink: 'تصغير',
    title: 'الفريق',
    sub: 'ما ينتظرك وما ينتظر الآخرين',
    light: 'فاتح',
    dark: 'داكن',
    mine: 'طلباتي',
    team: 'الجميع',
    cards: 'الأشخاص والوكلاء',
    graph: 'من يحتاج من',
    helpers: 'إظهار المساعدين',
    waitYou: 'بانتظارك',
    waitOthers: 'أنت تنتظر',
    nothing: 'لا شيء هنا. كل شيء تمام.',
    big: (n) => `${n} ${n === 1 ? 'طلب بانتظارك' : n === 2 ? 'طلبان بانتظارك' : n <= 10 ? 'طلبات بانتظارك' : 'طلبًا بانتظارك'}`,
    stale: (n) => `${n} أقدم من يومين`,
    allClear: 'أنت على اطلاع بكل شيء',
    hubNeeds: (n) => `تذاكر المركز التي تحتاجك: ${n}`,
    hubMore: (n) => `و${n} أخرى`,
    hubOpenTickets: 'افتح شاشة التذاكر',
    hubOffline: 'لا يمكن الوصول إلى المركز، فلا أستطيع معرفة إن كانت هناك تذاكر بانتظارك.',
    hubTokenMissing: 'لا يوجد رمز للمركز بعد، فلا أستطيع فحص تذاكره.',
    hubChecking: 'جارٍ فحص المركز…',
    hubOther: 'المركز لا يستجيب بشكل سليم، فلا أستطيع فحص تذاكره.',
    routeDefault: (t) => `All for One (الحالية): ${t}`,
    routeCoordinator: (t) => `المقترح للسؤال عنه: ${t} (المنسّق)`,
    routeAfo: (t) => `المنسّق غير متاح لـOne (غير نشط أو في مشروع آخر)، لذا أقترح أحدث جلسة All for One نشطة: ${t}`,
    routePin: (t) => `مثبّتة: ${t}`,
    routePinGone: 'الجلسة المثبّتة لم تعد نشطة، لذا أستخدم المقترحة.',
    routeNone: 'لا توجد جلسة All for One نشطة',
    pin: 'تثبيت',
    unpin: 'إلغاء التثبيت',
    working: 'يعمل على',
    onThem: 'بانتظاره',
    onOthers: 'ينتظر غيره',
    show: 'عرض التفاصيل',
    hide: 'إخفاء التفاصيل',
    done: 'تم',
    remind: 'ذكّر',
    reminded: 'تم التذكير',
    autoSent: 'أُرسل تذكير',
    ask: 'اسأل One',
    askAbout: 'اسأل One عن هذا',
    undo: 'تراجع',
    markedDone: 'تم الإنجاز',
    remSent: (n) => `أُرسل تذكير إلى ${n}`,
    access: 'وصول',
    approval: 'موافقة',
    ticket: 'تذكرة',
    long: 'ينتظر طويلًا',
    who: 'من ينتظره الجميع؟',
    whoSub: 'عدد الطلبات المفتوحة لكل شخص أو وكيل.',
    list: 'كل الطلبات المفتوحة',
    listSub: 'من يحتاج شيئًا، سهم، ومن يملكه.',
    you: 'أنت',
    active: 'يعمل',
    waiting: 'ينتظر أحدًا',
    projects: 'ما يعمل عليه الفريق',
    projectRole: 'مشروع',
    sessionRole: 'جلسة',
    emptyBoard: 'لا شيء ينتظر أحدًا \u{1F389}',
    emptyBoardSub: 'عندما تحتاج جلسة أو زميل شيئًا سيظهر هنا تلقائيًا.',
    emptyTitle: 'لا شيء على اللوحة بعد',
    emptyBody: 'تمتلئ اللوحة تلقائيًا أثناء عمل الفريق والجلسات. عُد بعد دقيقة.',
    emptySample: '',
    errTitle: 'تعذّرت قراءة جزء من اللوحة',
    errBody: 'سيحاول One مرة أخرى تلقائيًا. إذا استمر ذلك، اطلب من الفريق مراجعة قائمة الفريق.',
    skipped: (n) => `تم تخطي ${n} من الإدخالات (خاصة أو غير صالحة).`,
    loading: 'جارٍ التحميل...',
    needs: 'يحتاج',
    needsF: 'تحتاج',
    needYou: 'أنت',
    toWord: 'إلى',
    forWord: 'من أجل',
    toYou: 'إليك',
    ageF: (h) => {
      if (h < 1) return 'الآن';
      if (h < 24) return `منذ ${h === 1 ? 'ساعة' : h === 2 ? 'ساعتين' : h <= 10 ? `${h} ساعات` : `${h} ساعة`}`;
      const d = Math.round(h / 24);
      return `منذ ${d === 1 ? 'يوم' : d === 2 ? 'يومين' : d <= 10 ? `${d} أيام` : `${d} يومًا`}`;
    },
  },
};
