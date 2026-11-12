// SPDX-License-Identifier: GPL-2.0-only
// Bounded SPI transport only. FSL protocol/register/optical policy is userspace.
#include <linux/compat.h>
#include <linux/fs.h>
#include <linux/kref.h>
#include <linux/miscdevice.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/of.h>
#include <linux/slab.h>
#include <linux/spi/spi.h>
#include <linux/uaccess.h>

#include "fsl-uapi.h"

struct fsl_device {
	struct miscdevice misc;
	struct kref refs;
	/* Serializes remove against messages/open; opened supplies session-level
	 * exclusion without holding a mutex for a userspace session's lifetime.
	 */
	struct mutex lock;
	struct spi_device *spi;
	bool opened;
};

static void fsl_free(struct kref *refs)
{
	struct fsl_device *fsl = container_of(refs, struct fsl_device, refs);

	kfree(fsl->misc.name);
	kfree(fsl);
}

static int fsl_open(struct inode *inode, struct file *file)
{
	struct fsl_device *fsl = container_of(file->private_data,
					    struct fsl_device, misc);
	int ret = 0;

	mutex_lock(&fsl->lock);
	if (!fsl->spi) {
		ret = -ENODEV;
	} else if (fsl->opened) {
		ret = -EBUSY;
	} else {
		kref_get(&fsl->refs);
		fsl->opened = true;
		file->private_data = fsl;
	}
	mutex_unlock(&fsl->lock);
	if (!ret)
		return stream_open(inode, file);
	return ret;
}

static int fsl_release(struct inode *inode, struct file *file)
{
	struct fsl_device *fsl = file->private_data;

	mutex_lock(&fsl->lock);
	fsl->opened = false;
	mutex_unlock(&fsl->lock);
	kref_put(&fsl->refs, fsl_free);
	return 0;
}

static long fsl_ioctl(struct file *file, unsigned int command,
		      unsigned long argument)
{
	struct fsl_device *fsl = file->private_data;
	struct spi_transfer transfers[2] = {};
	struct spi_message message;
	struct fsl_transfer *request;
	u8 *tx = NULL, *rx = NULL;
	void __user *user = (void __user *)argument;
	int ret;

	if (command != FSL_IOC_TRANSFER)
		return -ENOTTY;
	request = memdup_user(user, sizeof(*request));
	if (IS_ERR(request))
		return PTR_ERR(request);
	/* Check each length before adding, to avoid overflow. No FSL opcode or
	 * address semantics belong here. Reserved fields must be zero.
	 */
	if (!request->tx_len || request->tx_len > FSL_MAX_TRANSFER ||
	    request->rx_len > FSL_MAX_TRANSFER ||
	    request->tx_len + request->rx_len > FSL_MAX_TRANSFER ||
	    request->reserved[0] || request->reserved[1]) {
		ret = -EINVAL;
		goto out;
	}
	memset(request->rx, 0, sizeof(request->rx));
	/* Separate kmalloc buffers retain DMA/cache alignment. The inline UAPI
	 * arrays are deliberately not used as DMA buffers.
	 */
	tx = kmemdup(request->tx, request->tx_len, GFP_KERNEL);
	if (request->rx_len)
		rx = kzalloc(request->rx_len, GFP_KERNEL);
	if (!tx || (request->rx_len && !rx)) {
		ret = -ENOMEM;
		goto out;
	}
	spi_message_init(&message);
	transfers[0].tx_buf = tx;
	transfers[0].len = request->tx_len;
	transfers[0].bits_per_word = 8;
	spi_message_add_tail(&transfers[0], &message);
	if (request->rx_len) {
		transfers[1].rx_buf = rx;
		transfers[1].len = request->rx_len;
		transfers[1].bits_per_word = 8;
		spi_message_add_tail(&transfers[1], &message);
	}
	/* Both transfers share ONE message; cs_change stays zero. The controller
	 * must support continuous CS, including a maximum-sized read.
	 */
	mutex_lock(&fsl->lock);
	if (!fsl->spi) {
		ret = -ENODEV;
	} else {
		ret = spi_sync(fsl->spi, &message);
		if (!ret && message.actual_length != request->tx_len + request->rx_len)
			ret = -EIO;
	}
	mutex_unlock(&fsl->lock);
	if (!ret && request->rx_len)
		memcpy(request->rx, rx, request->rx_len);
	if (!ret && copy_to_user(user, request, sizeof(*request)))
		ret = -EFAULT;
out:
	kfree(rx);
	kfree(tx);
	kfree(request);
	return ret;
}

#ifdef CONFIG_COMPAT
static long fsl_compat_ioctl(struct file *file, unsigned int command,
			     unsigned long argument)
{
	return fsl_ioctl(file, command, (unsigned long)compat_ptr(argument));
}
#endif

static const struct file_operations fsl_fops = {
	.owner = THIS_MODULE,
	.open = fsl_open,
	.release = fsl_release,
	.unlocked_ioctl = fsl_ioctl,
#ifdef CONFIG_COMPAT
	.compat_ioctl = fsl_compat_ioctl,
#endif
};

static int fsl_probe(struct spi_device *spi)
{
	struct fsl_device *fsl;
	int ret;

	/* Protocol transport settings. A verified board may impose a lower
	 * maximum frequency; FSL register encodings are independent of SPI mode.
	 */
	if (!spi->max_speed_hz)
		return -EINVAL;
	spi->mode = SPI_MODE_0;
	spi->bits_per_word = 8;
	spi->max_speed_hz = min(spi->max_speed_hz, 10000000U);
	ret = spi_setup(spi);
	if (ret)
		return ret;
	fsl = kzalloc(sizeof(*fsl), GFP_KERNEL);
	if (!fsl)
		return -ENOMEM;
	kref_init(&fsl->refs);
	mutex_init(&fsl->lock);
	fsl->spi = spi;
	fsl->misc.minor = MISC_DYNAMIC_MINOR;
	fsl->misc.name = kasprintf(GFP_KERNEL, "fsl-%s", dev_name(&spi->dev));
	fsl->misc.fops = &fsl_fops;
	fsl->misc.parent = &spi->dev;
	fsl->misc.mode = 0600;
	if (!fsl->misc.name) {
		ret = -ENOMEM;
		goto fail;
	}
	ret = misc_register(&fsl->misc);
	if (ret)
		goto fail;
	spi_set_drvdata(spi, fsl);
	return 0;
fail:
	kref_put(&fsl->refs, fsl_free);
	return ret;
}

static void fsl_remove(struct spi_device *spi)
{
	struct fsl_device *fsl = spi_get_drvdata(spi);

	/* No new opens; existing files keep their own ref. Wait for an active
	 * spi_sync before detaching the SPI pointer. Late ioctls return ENODEV.
	 */
	misc_deregister(&fsl->misc);
	mutex_lock(&fsl->lock);
	fsl->spi = NULL;
	mutex_unlock(&fsl->lock);
	kref_put(&fsl->refs, fsl_free);
}

static const struct of_device_id fsl_of_match[] = {
	{ .compatible = "fiberhome,an4475-g2-fsl" },
	{}
};
MODULE_DEVICE_TABLE(of, fsl_of_match);

static const struct spi_device_id fsl_spi_ids[] = {
	{ "an4475-g2-fsl", 0 },
	{}
};
MODULE_DEVICE_TABLE(spi, fsl_spi_ids);

static struct spi_driver fsl_driver = {
	.driver = {
		.name = "fiberhome-fsl",
		.of_match_table = fsl_of_match,
	},
	.probe = fsl_probe,
	.remove = fsl_remove,
	.id_table = fsl_spi_ids,
};
module_spi_driver(fsl_driver);

MODULE_DESCRIPTION("FiberHome FSL exclusive SPI transport (protocol in userspace)");
MODULE_AUTHOR("openwrt-fttr contributors");
MODULE_LICENSE("GPL");
