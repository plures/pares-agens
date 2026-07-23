/**
 * bitnet_shim.cpp — Maps bitnet_* API onto llama.cpp internals.
 *
 * Targets the llama.cpp version bundled with Microsoft/BitNet.
 */

#include "bitnet_shim.h"
#include "llama.h"
#include <cstring>
#include <cstdlib>
#include <vector>
#include <thread>

struct BitNetModel {
    llama_model *model;
};

struct BitNetContext {
    const BitNetModel *owner;
    llama_context *ctx;
    llama_sampler *smpl;
    int n_past;
};

/* ── Model ─────────────────────────────────────────────────────────────── */

BitNetModel *bitnet_model_load(const char *model_path) {
    llama_model_params params = llama_model_default_params();
    params.n_gpu_layers = 0; /* CPU only — the whole point of BitNet */

    llama_model *m = llama_load_model_from_file(model_path, params);
    if (!m) return nullptr;

    auto *bm = new BitNetModel;
    bm->model = m;
    return bm;
}

void bitnet_model_free(BitNetModel *model) {
    if (!model) return;
    llama_free_model(model->model);
    delete model;
}

/* ── Context ───────────────────────────────────────────────────────────── */

BitNetContext *bitnet_context_create(const BitNetModel *model) {
    if (!model) return nullptr;

    llama_context_params params = llama_context_default_params();
    params.n_ctx = 4096;
    params.n_batch = 512;
    
    /* Use all available CPU cores for maximum throughput */
    int n_cores = std::thread::hardware_concurrency();
    if (n_cores <= 0) n_cores = 4;
    params.n_threads = n_cores;
    params.n_threads_batch = n_cores;

    llama_context *ctx = llama_new_context_with_model(
        const_cast<llama_model *>(model->model), params);
    if (!ctx) return nullptr;

    /* Default sampler: temperature + top-p */
    llama_sampler *smpl = llama_sampler_chain_init(llama_sampler_chain_default_params());
    llama_sampler_chain_add(smpl, llama_sampler_init_temp(0.7f));
    llama_sampler_chain_add(smpl, llama_sampler_init_top_p(0.9f, 1));
    llama_sampler_chain_add(smpl, llama_sampler_init_dist(42));

    auto *bc = new BitNetContext;
    bc->owner = model;
    bc->ctx = ctx;
    bc->smpl = smpl;
    bc->n_past = 0;
    return bc;
}

void bitnet_context_free(BitNetContext *ctx) {
    if (!ctx) return;
    if (ctx->smpl) llama_sampler_free(ctx->smpl);
    if (ctx->ctx) llama_free(ctx->ctx);
    delete ctx;
}

void bitnet_context_reset(BitNetContext *ctx) {
    if (!ctx || !ctx->ctx) return;
    llama_kv_cache_clear(ctx->ctx);
    if (ctx->smpl) llama_sampler_reset(ctx->smpl);
    ctx->n_past = 0;
}

/* ── Tokenization ──────────────────────────────────────────────────────── */

int bitnet_tokenize(BitNetContext *ctx, const char *text, int *tokens, int max_tokens) {
    if (!ctx || !ctx->owner) return -1;
    const llama_model *model = ctx->owner->model;

    int n = llama_tokenize(model, text, (int)strlen(text),
                           (llama_token *)tokens, max_tokens,
                           /* add_special */ true, /* parse_special */ false);
    return n;
}

int bitnet_token_to_piece(BitNetContext *ctx, int token, char *buf, int buf_size) {
    if (!ctx || !ctx->owner) return -1;
    const llama_model *model = ctx->owner->model;

    int n = llama_token_to_piece(model, (llama_token)token, buf, buf_size, 0, false);
    return n;
}

/* ── Inference ─────────────────────────────────────────────────────────── */

int bitnet_eval(BitNetContext *ctx, const int *tokens, int n_tokens) {
    if (!ctx || !ctx->ctx) return -1;

    llama_batch batch = llama_batch_get_one(
        const_cast<llama_token *>((const llama_token *)tokens),
        n_tokens,
        ctx->n_past, /* pos_0 */
        0            /* seq_id */
    );

    if (llama_decode(ctx->ctx, batch)) {
        return -1;
    }
    ctx->n_past += n_tokens;
    return 0;
}

int bitnet_sample(BitNetContext *ctx, const BitNetGenParams *params) {
    if (!ctx || !ctx->ctx || !ctx->smpl) return -1;

    /* Reconfigure sampler if params changed */
    if (params) {
        llama_sampler_free(ctx->smpl);
        ctx->smpl = llama_sampler_chain_init(llama_sampler_chain_default_params());
        llama_sampler_chain_add(ctx->smpl, llama_sampler_init_temp(params->temperature));
        llama_sampler_chain_add(ctx->smpl, llama_sampler_init_top_p(params->top_p, 1));
        int seed = params->seed >= 0 ? params->seed : 42;
        llama_sampler_chain_add(ctx->smpl, llama_sampler_init_dist(seed));
    }

    /* Sample from the last position's logits */
    int n_vocab = llama_n_vocab(ctx->owner->model);
    float *logits = llama_get_logits(ctx->ctx);

    /* Build token_data_array */
    llama_token_data_array candidates;
    std::vector<llama_token_data> candidates_data(n_vocab);
    for (int i = 0; i < n_vocab; i++) {
        candidates_data[i] = { i, logits[i], 0.0f };
    }
    candidates.data = candidates_data.data();
    candidates.size = n_vocab;
    candidates.sorted = false;
    candidates.selected = -1;

    llama_sampler_apply(ctx->smpl, &candidates);

    int id = candidates.data[candidates.selected].id;

    /* Check for end-of-generation tokens */
    const llama_model *model = ctx->owner->model;
    if (llama_token_is_eog(model, id)) {
        return -1; /* BITNET_TOKEN_EOS */
    }

    return id;
}
