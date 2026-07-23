/**
 * bitnet_shim.h — Thin C API wrapping llama.cpp for pares-radix FFI.
 *
 * This maps our stable ABI (bitnet_model_load, bitnet_context_create, etc.)
 * onto the underlying llama.cpp API, which builds with BitNet kernel support
 * when compiled from the Microsoft/BitNet repo.
 */

#ifndef BITNET_SHIM_H
#define BITNET_SHIM_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct BitNetModel BitNetModel;
typedef struct BitNetContext BitNetContext;

typedef struct {
    float temperature;
    float top_p;
    int seed;
    int n_predict;
    int n_threads;
} BitNetGenParams;

/* Model lifecycle */
BitNetModel *bitnet_model_load(const char *model_path);
void bitnet_model_free(BitNetModel *model);

/* Context lifecycle */
BitNetContext *bitnet_context_create(const BitNetModel *model);
void bitnet_context_free(BitNetContext *ctx);
void bitnet_context_reset(BitNetContext *ctx);

/* Tokenization */
int bitnet_tokenize(BitNetContext *ctx, const char *text, int *tokens, int max_tokens);
int bitnet_token_to_piece(BitNetContext *ctx, int token, char *buf, int buf_size);

/* Inference */
int bitnet_eval(BitNetContext *ctx, const int *tokens, int n_tokens);
int bitnet_sample(BitNetContext *ctx, const BitNetGenParams *params);

#ifdef __cplusplus
}
#endif

#endif /* BITNET_SHIM_H */
