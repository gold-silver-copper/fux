/*
 * A flat C interface to libvterm 0.3.3 for fux-vt-compare, so the Rust side
 * declares only plain structs of ints and never libvterm's bitfields.
 * src/engines/libvterm.rs says how the engine is driven and read.
 *
 * Besides libvterm's public API, the shim uses vterm_internal.h, the
 * header of the very source it is compiled with: it reads the modes and
 * the pending-wrap ("phantom") flag in VTermState, which the public API
 * does not expose; it stands between the parser and the state, and
 * between the state and the screen, by wrapping the callbacks each
 * installs, to learn which scrollback rows were soft-wrapped (see
 * FvcTerm.shadow) and to keep libvterm from crashing or hanging; and,
 * for the same reason, it corrects two fields before and after a resize.
 * libvterm.rs lists each of these steps.
 */
#include "vterm.h"
#include "vterm_internal.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Called instead of abort() and fprintf() inside libvterm (build.rs
 * force-includes a header that renames them). The only live call of
 * either, in screen.c's resize_buffer, is "screen_resize failed to update
 * cursor position", where libvterm aborts the process; returning instead
 * leaves the cursor at (0, 0). */
void fux_vt_compare_libvterm_abort(void) {}

int fux_vt_compare_libvterm_fprintf(FILE *stream, const char *format, ...)
{
  (void)stream;
  (void)format;
  return 0;
}

/* A colour: kind 0 the default, 1 a palette index, 2 direct colour. */
typedef struct {
  int32_t kind;
  uint8_t index, red, green, blue;
} FvcColor;

/* A cell. width is 1 or 2, or 0 for the second half of a wide glyph. */
typedef struct {
  uint32_t chars[VTERM_MAX_CHARS_PER_CELL];
  int32_t count;
  int32_t width;
  int32_t bold, underline, italic, blink, reverse, conceal, strike;
  FvcColor fg, bg;
} FvcCell;

typedef struct {
  int32_t rows, cols, cursor_row, cursor_col;
  int32_t pending_wrap, cursor_visible, autowrap, origin, alternate;
  int32_t application_cursor, application_keypad, bracketed_paste, focus_reporting;
} FvcState;

/* A row pushed to scrollback, as libvterm gave it, but for its trailing
 * blank cells of no attribute and default colours: only `used` of its
 * `cols` cells are kept, and the rest are made again when read. */
typedef struct {
  int cols;
  int used;
  int wrapped;
  VTermScreenCell *cells;
} FvcLine;

typedef struct {
  char *bytes;
  size_t len, cap;
} FvcBytes;

typedef struct FvcTerm {
  VTerm *vt;
  VTermState *state;
  VTermScreen *screen;

  /* The state callbacks the screen installed, and ours around them. */
  const VTermStateCallbacks *inner;
  void *inner_data;
  VTermStateCallbacks outer;

  /* The parser callbacks the state installed, and ours around them. */
  const VTermParserCallbacks *parser_inner;
  void *parser_data;
  VTermParserCallbacks parser_outer;

  /* The continuation flags of the primary screen's rows, as of the last
   * state callback. libvterm moves its own flags before telling the
   * screen to scroll, so when the screen pushes rows to scrollback the
   * flags of all but the last pushed row's successor are gone; these are
   * the flags from before the move. Row y is soft-wrapped when row y+1
   * is a continuation. */
  unsigned char *shadow;
  int shadow_rows;
  /* The screen row the next pushed line comes from: pushes come in
   * batches of rows 0, 1, 2... from one scroll or one resize. */
  int batch;

  /* Scrollback: a ring of at most `limit` lines; `head` is the oldest. */
  FvcLine *lines;
  size_t cap, head, count, limit;

  FvcBytes replies;
  FvcBytes title;
  FvcBytes title_pending;

  int cursor_visible;
  int alternate;
  int reverse;
} FvcTerm;

static void bytes_append(FvcBytes *b, const char *s, size_t len)
{
  if(len == 0)
    return;
  if(b->len + len > b->cap) {
    size_t cap = b->cap ? b->cap : 64;
    while(cap < b->len + len)
      cap *= 2;
    char *grown = realloc(b->bytes, cap);
    if(!grown)
      return;
    b->bytes = grown;
    b->cap = cap;
  }
  memcpy(b->bytes + b->len, s, len);
  b->len += len;
}

static void shadow_sync(FvcTerm *t)
{
  int rows = t->state->rows;
  if(rows != t->shadow_rows) {
    unsigned char *grown = realloc(t->shadow, (size_t)rows);
    if(!grown)
      return;
    t->shadow = grown;
    t->shadow_rows = rows;
  }
  const VTermLineInfo *info = t->state->lineinfos[BUFIDX_PRIMARY];
  for(int row = 0; row < rows; row++)
    t->shadow[row] = info[row].continuation;
}

/* ---- State callbacks, around the screen's ---- */

static int outer_putglyph(VTermGlyphInfo *info, VTermPos pos, void *user)
{
  FvcTerm *t = user;
  /* libvterm writes a wide glyph's second half to the next cell without
   * checking it exists: past the row (a one-column terminal, or REP of a
   * wide glyph in the last column), that is a NULL dereference. Such a
   * glyph is dropped instead, as Ghostty and fux-vt drop it. */
  if(pos.col >= 0 && pos.col + info->width > t->state->cols)
    return 1;
  int done = t->inner->putglyph(info, pos, t->inner_data);
  if(pos.row >= 0 && pos.row < t->shadow_rows)
    t->shadow[pos.row] = t->state->lineinfos[BUFIDX_PRIMARY][pos.row].continuation;
  return done;
}

static int outer_erase(VTermRect rect, int selective, void *user)
{
  FvcTerm *t = user;
  int done = t->inner->erase(rect, selective, t->inner_data);
  shadow_sync(t);
  return done;
}

static int outer_scrollrect(VTermRect rect, int downward, int rightward, void *user)
{
  FvcTerm *t = user;
  t->batch = 0;
  int done = t->inner->scrollrect(rect, downward, rightward, t->inner_data);
  t->batch = -1;
  shadow_sync(t);
  return done;
}

/* ---- Parser callbacks, around the state's ---- */

static int outer_csi(const char *leader, const long args[], int argcount, const char *intermed, char command, void *user)
{
  FvcTerm *t = user;
  /* REP repeats the last glyph by stepping the cursor its width at a
   * time: with none printed yet, or one of no width, it never ends.
   * Such a REP is ignored, as xterm ignores a REP with nothing before it. */
  if(command == 'b' && !leader && !intermed && t->state->combine_width <= 0)
    return 1;
  return t->parser_inner->csi(leader, args, argcount, intermed, command, t->parser_data);
}

static int outer_text(const char *bytes, size_t len, void *user)
{
  FvcTerm *t = user;
  return t->parser_inner->text(bytes, len, t->parser_data);
}

static int outer_control(unsigned char control, void *user)
{
  FvcTerm *t = user;
  return t->parser_inner->control(control, t->parser_data);
}

static int outer_escape(const char *bytes, size_t len, void *user)
{
  FvcTerm *t = user;
  return t->parser_inner->escape(bytes, len, t->parser_data);
}

static int outer_osc(int command, VTermStringFragment frag, void *user)
{
  FvcTerm *t = user;
  return t->parser_inner->osc(command, frag, t->parser_data);
}

static int outer_dcs(const char *command, size_t commandlen, VTermStringFragment frag, void *user)
{
  FvcTerm *t = user;
  return t->parser_inner->dcs(command, commandlen, frag, t->parser_data);
}

static int outer_apc(VTermStringFragment frag, void *user)
{
  FvcTerm *t = user;
  return t->parser_inner->apc(frag, t->parser_data);
}

static int outer_pm(VTermStringFragment frag, void *user)
{
  FvcTerm *t = user;
  return t->parser_inner->pm(frag, t->parser_data);
}

static int outer_sos(VTermStringFragment frag, void *user)
{
  FvcTerm *t = user;
  return t->parser_inner->sos(frag, t->parser_data);
}

static int outer_parser_resize(int rows, int cols, void *user)
{
  FvcTerm *t = user;
  return t->parser_inner->resize(rows, cols, t->parser_data);
}

/* The rest only pass the screen its own `user`. */

static int outer_movecursor(VTermPos pos, VTermPos oldpos, int visible, void *user)
{
  FvcTerm *t = user;
  return t->inner->movecursor ? t->inner->movecursor(pos, oldpos, visible, t->inner_data) : 0;
}

static int outer_moverect(VTermRect dest, VTermRect src, void *user)
{
  FvcTerm *t = user;
  return t->inner->moverect ? t->inner->moverect(dest, src, t->inner_data) : 0;
}

static int outer_initpen(void *user)
{
  FvcTerm *t = user;
  return t->inner->initpen ? t->inner->initpen(t->inner_data) : 0;
}

static int outer_setpenattr(VTermAttr attr, VTermValue *val, void *user)
{
  FvcTerm *t = user;
  return t->inner->setpenattr ? t->inner->setpenattr(attr, val, t->inner_data) : 0;
}

static int outer_settermprop(VTermProp prop, VTermValue *val, void *user)
{
  FvcTerm *t = user;
  return t->inner->settermprop ? t->inner->settermprop(prop, val, t->inner_data) : 0;
}

static int outer_bell(void *user)
{
  FvcTerm *t = user;
  return t->inner->bell ? t->inner->bell(t->inner_data) : 0;
}

static int outer_resize(int rows, int cols, VTermStateFields *fields, void *user)
{
  FvcTerm *t = user;
  return t->inner->resize ? t->inner->resize(rows, cols, fields, t->inner_data) : 0;
}

static int outer_setlineinfo(int row, const VTermLineInfo *newinfo, const VTermLineInfo *oldinfo, void *user)
{
  FvcTerm *t = user;
  return t->inner->setlineinfo ? t->inner->setlineinfo(row, newinfo, oldinfo, t->inner_data) : 0;
}

static int outer_sb_clear(void *user)
{
  FvcTerm *t = user;
  return t->inner->sb_clear ? t->inner->sb_clear(t->inner_data) : 0;
}

/* ---- Screen callbacks ---- */

static void line_free(FvcLine *line)
{
  free(line->cells);
  line->cells = NULL;
}

/* An erased cell of no attribute and default colours, as blank() makes. */
static int plain_blank(const VTermScreenCell *c)
{
  const VTermScreenCellAttrs *a = &c->attrs;
  return c->chars[0] == 0 && c->width == 1 &&
         !a->bold && !a->underline && !a->italic && !a->blink && !a->reverse &&
         !a->conceal && !a->strike && !a->font && !a->dwl && !a->dhl &&
         !a->small && !a->baseline &&
         VTERM_COLOR_IS_DEFAULT_FG(&c->fg) && VTERM_COLOR_IS_DEFAULT_BG(&c->bg);
}

static void blank(const FvcTerm *t, VTermScreenCell *cell)
{
  memset(cell, 0, sizeof(*cell));
  cell->width = 1;
  vterm_state_get_default_colors(t->state, &cell->fg, &cell->bg);
  cell->fg.type |= VTERM_COLOR_DEFAULT_FG;
  cell->bg.type |= VTERM_COLOR_DEFAULT_BG;
}

static int on_pushline(int cols, const VTermScreenCell *cells, void *user)
{
  FvcTerm *t = user;
  int row = t->batch;
  int wrapped = 0;
  if(row >= 0) {
    if(row + 1 < t->shadow_rows)
      wrapped = t->shadow[row + 1];
    t->batch++;
  }

  if(t->count == t->cap && t->cap < t->limit) {
    size_t cap = t->cap ? t->cap * 2 : 256;
    if(cap > t->limit)
      cap = t->limit;
    FvcLine *grown = malloc(cap * sizeof(FvcLine));
    if(!grown)
      return 0;
    for(size_t i = 0; i < t->count; i++)
      grown[i] = t->lines[(t->head + i) % t->cap];
    free(t->lines);
    t->lines = grown;
    t->cap = cap;
    t->head = 0;
  }
  if(t->cap == 0)
    return 0;

  int used = cols;
  while(used > 0 && plain_blank(&cells[used - 1]))
    used--;
  VTermScreenCell *copy = NULL;
  if(used > 0) {
    copy = malloc((size_t)used * sizeof(VTermScreenCell));
    if(!copy)
      return 0;
    memcpy(copy, cells, (size_t)used * sizeof(VTermScreenCell));
  }

  FvcLine *slot;
  if(t->count == t->cap) {
    /* Full: the oldest line makes way. */
    slot = &t->lines[t->head];
    line_free(slot);
    t->head = (t->head + 1) % t->cap;
  }
  else {
    slot = &t->lines[(t->head + t->count) % t->cap];
    t->count++;
  }
  slot->cols = cols;
  slot->used = used;
  slot->wrapped = wrapped;
  slot->cells = copy;
  return 1;
}

/* libvterm steps through the popped row by each cell's width, so every
 * cell given back has a width of 1 or 2. */
static int on_popline(int cols, VTermScreenCell *cells, void *user)
{
  FvcTerm *t = user;
  if(t->count == 0)
    return 0;
  FvcLine *line = &t->lines[(t->head + t->count - 1) % t->cap];
  for(int col = 0; col < cols; col++) {
    if(col < line->used) {
      cells[col] = line->cells[col];
      if(cells[col].width < 1)
        cells[col].width = 1;
    }
    else
      blank(t, &cells[col]);
  }
  line_free(line);
  t->count--;
  return 1;
}

static int on_sb_clear(void *user)
{
  FvcTerm *t = user;
  for(size_t i = 0; i < t->count; i++)
    line_free(&t->lines[(t->head + i) % t->cap]);
  t->count = 0;
  t->head = 0;
  return 1;
}

static int on_settermprop(VTermProp prop, VTermValue *val, void *user)
{
  FvcTerm *t = user;
  switch(prop) {
  case VTERM_PROP_CURSORVISIBLE:
    t->cursor_visible = val->boolean;
    break;
  case VTERM_PROP_ALTSCREEN:
    t->alternate = val->boolean;
    break;
  case VTERM_PROP_REVERSE:
    t->reverse = val->boolean;
    break;
  case VTERM_PROP_TITLE:
    /* 0.3 delivers a title in fragments, the first marked initial and
     * the last final. */
    if(val->string.initial)
      t->title_pending.len = 0;
    bytes_append(&t->title_pending, val->string.str, val->string.len);
    if(val->string.final) {
      t->title.len = 0;
      bytes_append(&t->title, t->title_pending.bytes, t->title_pending.len);
    }
    break;
  case VTERM_PROP_CURSORBLINK:
  case VTERM_PROP_ICONNAME:
  case VTERM_PROP_CURSORSHAPE:
  case VTERM_PROP_MOUSE:
  case VTERM_PROP_FOCUSREPORT:
  case VTERM_N_PROPS:
    break;
  }
  /* Accepted: libvterm stores a property only when this says yes. */
  return 1;
}

static const VTermScreenCallbacks screen_callbacks = {
  .settermprop = on_settermprop,
  .sb_pushline = on_pushline,
  .sb_popline  = on_popline,
  .sb_clear    = on_sb_clear,
};

static void on_output(const char *s, size_t len, void *user)
{
  FvcTerm *t = user;
  bytes_append(&t->replies, s, len);
}

/* ---- The interface ---- */

FvcTerm *fvc_libvterm_new(int rows, int cols, size_t history_limit)
{
  if(rows < 1 || cols < 1)
    return NULL;
  FvcTerm *t = calloc(1, sizeof(FvcTerm));
  if(!t)
    return NULL;
  t->batch = -1;
  t->limit = history_limit;
  t->cursor_visible = 1;

  t->vt = vterm_new(rows, cols);
  if(!t->vt) {
    free(t);
    return NULL;
  }
  vterm_set_utf8(t->vt, 1);
  vterm_output_set_callback(t->vt, on_output, t);

  t->screen = vterm_obtain_screen(t->vt);
  t->state = vterm_obtain_state(t->vt);
  vterm_screen_enable_altscreen(t->screen, 1);
  vterm_screen_enable_reflow(t->screen, true);
  /* As Neovim sets it: damage is merged, and scrolls are not replayed. */
  vterm_screen_set_damage_merge(t->screen, VTERM_DAMAGE_SCROLL);
  vterm_screen_set_callbacks(t->screen, &screen_callbacks, t);

  t->inner = t->state->callbacks;
  t->inner_data = t->state->cbdata;
  /* Every callback the screen set goes through ours, which hands the
   * screen its own `user`; one it left unset stays unset. */
  const VTermStateCallbacks *in = t->inner;
  t->outer = (VTermStateCallbacks){
    .putglyph    = in->putglyph    ? outer_putglyph    : NULL,
    .movecursor  = in->movecursor  ? outer_movecursor  : NULL,
    .scrollrect  = in->scrollrect  ? outer_scrollrect  : NULL,
    .moverect    = in->moverect    ? outer_moverect    : NULL,
    .erase       = in->erase       ? outer_erase       : NULL,
    .initpen     = in->initpen     ? outer_initpen     : NULL,
    .setpenattr  = in->setpenattr  ? outer_setpenattr  : NULL,
    .settermprop = in->settermprop ? outer_settermprop : NULL,
    .bell        = in->bell        ? outer_bell        : NULL,
    .resize      = in->resize      ? outer_resize      : NULL,
    .setlineinfo = in->setlineinfo ? outer_setlineinfo : NULL,
    .sb_clear    = in->sb_clear    ? outer_sb_clear    : NULL,
  };
  vterm_state_set_callbacks(t->state, &t->outer, t);

  t->parser_inner = t->vt->parser.callbacks;
  t->parser_data = t->vt->parser.cbdata;
  const VTermParserCallbacks *pin = t->parser_inner;
  t->parser_outer = (VTermParserCallbacks){
    .text    = pin->text    ? outer_text          : NULL,
    .control = pin->control ? outer_control       : NULL,
    .escape  = pin->escape  ? outer_escape        : NULL,
    .csi     = pin->csi     ? outer_csi           : NULL,
    .osc     = pin->osc     ? outer_osc           : NULL,
    .dcs     = pin->dcs     ? outer_dcs           : NULL,
    .apc     = pin->apc     ? outer_apc           : NULL,
    .pm      = pin->pm      ? outer_pm            : NULL,
    .sos     = pin->sos     ? outer_sos           : NULL,
    .resize  = pin->resize  ? outer_parser_resize : NULL,
  };
  vterm_parser_set_callbacks(t->vt, &t->parser_outer, t);

  vterm_screen_reset(t->screen, 1);
  shadow_sync(t);
  return t;
}

void fvc_libvterm_free(FvcTerm *t)
{
  if(!t)
    return;
  on_sb_clear(t);
  free(t->lines);
  free(t->shadow);
  free(t->replies.bytes);
  free(t->title.bytes);
  free(t->title_pending.bytes);
  vterm_free(t->vt);
  free(t);
}

void fvc_libvterm_write(FvcTerm *t, const char *bytes, size_t len)
{
  vterm_input_write(t->vt, bytes, len);
}

void fvc_libvterm_resize(FvcTerm *t, int rows, int cols)
{
  /* When the top row continues a line that scrolled off, resize_buffer
   * looks for that line's start above row 0 and reads before its buffer.
   * The top row is made the start of its line instead. */
  for(int buf = BUFIDX_PRIMARY; buf <= BUFIDX_ALTSCREEN; buf++)
    if(t->state->lineinfos[buf] && t->state->rows > 0)
      t->state->lineinfos[buf][0].continuation = 0;
  shadow_sync(t);
  t->batch = 0;
  vterm_set_size(t->vt, rows, cols);
  t->batch = -1;
  shadow_sync(t);
  /* A resize leaves the cursor DECSC saved where it was, and DECRC puts it
   * back there unchecked: past a smaller screen, where the next glyph
   * writes past the row flags. It is kept on the screen instead. */
  VTermPos *saved = &t->state->saved.pos;
  if(saved->row >= t->state->rows)
    saved->row = t->state->rows - 1;
  if(saved->col >= t->state->cols)
    saved->col = t->state->cols - 1;
}

void fvc_libvterm_state(const FvcTerm *t, FvcState *out)
{
  const VTermState *s = t->state;
  VTermPos pos;
  vterm_state_get_cursorpos(s, &pos);
  out->rows = s->rows;
  out->cols = s->cols;
  out->cursor_row = pos.row;
  out->cursor_col = pos.col;
  out->pending_wrap = s->at_phantom;
  out->cursor_visible = t->cursor_visible;
  out->autowrap = s->mode.autowrap;
  out->origin = s->mode.origin;
  out->alternate = t->alternate;
  out->application_cursor = s->mode.cursor;
  out->application_keypad = s->mode.keypad;
  out->bracketed_paste = s->mode.bracketpaste;
  out->focus_reporting = s->mode.report_focus;
}

static void flatten_color(const VTermColor *c, FvcColor *out)
{
  memset(out, 0, sizeof(*out));
  if(VTERM_COLOR_IS_DEFAULT_FG(c) || VTERM_COLOR_IS_DEFAULT_BG(c))
    out->kind = 0;
  else if(VTERM_COLOR_IS_INDEXED(c)) {
    out->kind = 1;
    out->index = c->indexed.idx;
  }
  else {
    out->kind = 2;
    out->red = c->rgb.red;
    out->green = c->rgb.green;
    out->blue = c->rgb.blue;
  }
}

static void flatten(const VTermScreenCell *c, int reverse, FvcCell *out)
{
  memset(out, 0, sizeof(*out));
  if(c->chars[0] == (uint32_t)-1) {
    /* The second half of a wide glyph. */
    out->width = 0;
    return;
  }
  int count = 0;
  while(count < VTERM_MAX_CHARS_PER_CELL && c->chars[count]) {
    out->chars[count] = c->chars[count];
    count++;
  }
  out->count = count;
  out->width = c->width == 2 ? 2 : 1;
  out->bold = c->attrs.bold;
  out->underline = c->attrs.underline;
  out->italic = c->attrs.italic;
  out->blink = c->attrs.blink;
  out->reverse = c->attrs.reverse ^ (reverse ? 1 : 0);
  out->conceal = c->attrs.conceal;
  out->strike = c->attrs.strike;
  flatten_color(&c->fg, &out->fg);
  flatten_color(&c->bg, &out->bg);
}

int fvc_libvterm_cell(const FvcTerm *t, int row, int col, FvcCell *out)
{
  VTermScreenCell cell;
  memset(&cell, 0, sizeof(cell));
  VTermPos pos = { .row = row, .col = col };
  if(row < 0 || row >= t->state->rows || col < 0 || col >= t->state->cols)
    return 0;
  if(!vterm_screen_get_cell(t->screen, pos, &cell))
    return 0;
  /* vterm_screen_get_cell folds DECSCNM (reverse video for the whole
   * screen) into every cell; the cell's own attribute is wanted. */
  flatten(&cell, t->reverse, out);
  return 1;
}

int fvc_libvterm_row_wrapped(const FvcTerm *t, int row)
{
  if(row < 0 || row + 1 >= t->state->rows)
    return 0;
  return vterm_state_get_lineinfo(t->state, row + 1)->continuation;
}

size_t fvc_libvterm_history_len(const FvcTerm *t)
{
  return t->count;
}

static const FvcLine *history_line(const FvcTerm *t, size_t index)
{
  if(index >= t->count)
    return NULL;
  return &t->lines[(t->head + index) % t->cap];
}

int fvc_libvterm_history_cols(const FvcTerm *t, size_t index)
{
  const FvcLine *line = history_line(t, index);
  return line ? line->cols : 0;
}

int fvc_libvterm_history_wrapped(const FvcTerm *t, size_t index)
{
  const FvcLine *line = history_line(t, index);
  return line ? line->wrapped : 0;
}

int fvc_libvterm_history_cell(const FvcTerm *t, size_t index, int col, FvcCell *out)
{
  const FvcLine *line = history_line(t, index);
  if(!line || col < 0 || col >= line->cols)
    return 0;
  if(col < line->used)
    flatten(&line->cells[col], 0, out);
  else {
    VTermScreenCell cell;
    blank(t, &cell);
    flatten(&cell, 0, out);
  }
  return 1;
}

const char *fvc_libvterm_title(const FvcTerm *t, size_t *len)
{
  *len = t->title.len;
  return t->title.bytes;
}

const char *fvc_libvterm_replies(const FvcTerm *t, size_t *len)
{
  *len = t->replies.len;
  return t->replies.bytes;
}
